// 壁纸配置弹窗（对标 Wallpaper Engine 的「壁纸配置」窗口），两个分区：
//
// 「作者属性」= project.json general.properties
//   - 打开时读取（library_item_props，含本地化文案/wire 值）；没声明可配置项时
//     明确展示「无可配置项」状态
//   - 按类型渲染控件（color/bool/slider/combo/textinput/file，text 为分节标题），
//     按 condition 对当前草稿求值决定显隐（与 WE 一致：隐藏项的值仍下发）
//   - 改动即生效：500ms 防抖自动保存，关闭前冲刷未保存草稿
//   - 支持搜索（真实壁纸常有 100+ 项）、单项恢复默认、全部恢复默认
//
// 「播放设置」= 每壁纸的显示模式/清晰度/帧率/音量（settings 表 play_cfg:{item}）
//   - WE 的这几项是**按壁纸记忆**的，不是全局单例；未设置的项显示「跟随全局」
//   - 与作者属性分开保存：存储位置与生效路径都不同（前者要热更属性/重挂，
//     后者只需 setFit / setRenderDpr / setSceneFps / setVolume）
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  api,
  type ItemPlayConfig,
  type PlayConfigGlobals,
  type WebPropDef,
  type WebPropValues,
} from "../api/steam";
import { evalCondition } from "../lib/weCondition";
import { refreshItemPropsCache } from "../hooks/useItemProps";
import { ConfirmModal } from "./ConfirmModal";
import { EmptyState } from "./EmptyState";
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

/**
 * 壁纸配置面板（纯内容：标题/Tab/属性/底栏），填满父容器，不含遮罩与定位。
 *
 * 两种承载方式共用这一个组件：
 * - 主窗口内弹窗（WallpaperPropsModal）：Portal + 居中遮罩 + h-[78vh] 卡片
 * - 独立设置窗口（props-main.tsx）：直接铺满整个窗口
 * `embedded` 区分两者的尺寸/边框：独立窗口里它就是窗口本体，不要卡片阴影与限宽。
 */
export function WallpaperPropsPanel({
  itemId,
  title,
  onClose,
  embedded = false,
}: {
  itemId: string;
  title: string;
  onClose: () => void;
  /** 独立窗口承载：铺满，去掉卡片样式与限高 */
  embedded?: boolean;
}) {
  const [defs, setDefs] = useState<WebPropDef[] | null>(null); // null = 读取中
  const [loadError, setLoadError] = useState("");
  const [draft, setDraft] = useState<WebPropValues>({});
  const [query, setQuery] = useState("");
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [errMsg, setErrMsg] = useState("");
  const [confirmReset, setConfirmReset] = useState(false);
  /** 当前分区。作者属性为空的壁纸直接落到播放设置（那才是它唯一能配的东西） */
  const [tab, setTab] = useState<"props" | "play">("props");
  /** 每壁纸播放设置覆盖（字段缺失 = 跟随全局）+ 当前全局默认 */
  const [play, setPlay] = useState<ItemPlayConfig>({});
  const [globals, setGlobals] = useState<PlayConfigGlobals | null>(null);
  const [playMsg, setPlayMsg] = useState("");

  const draftRef = useRef(draft);
  draftRef.current = draft;
  const defsRef = useRef<WebPropDef[] | null>(null);
  defsRef.current = defs;
  const saveTimer = useRef<number | null>(null);

  // ---- 独立窗口拖动：交给系统背景拖动（movableByWindowBackground，见
  // props_window.rs）。JS 侧不要再插手——WKWebView 在非交互区域会吃掉鼠标
  // 事件走原生手势，两套并存就是"时灵时不灵"。双击最大化保留。 ----

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

  // ---- 内容服务器地址：拼 file 属性（图片）的缩略图 URL ----
  // 壁纸包内文件统一经 /media/<token>/<item>/<rel> 提供，与壁纸类型无关
  // （/web 只是站点根，/media 也能读到同一目录）。
  const [fileBase, setFileBase] = useState<string>("");
  useEffect(() => {
    let alive = true;
    api
      .contentServerStatus()
      .then((s) => {
        if (alive && s.token) setFileBase(`${s.base}/media/${s.token}/${itemId}/`);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [itemId]);

  /** file 属性值 → 可访问 URL；空值/外部 URL 返回空串 */
  const fileUrl = useCallback(
    (val: unknown) => {
      const rel = String(val ?? "").trim();
      if (!rel || !fileBase) return "";
      if (/^https?:\/\//i.test(rel) || rel.startsWith("data:")) return rel;
      return fileBase + rel.replace(/^\/+/, "");
    },
    [fileBase],
  );

  // ---- 载入每壁纸播放设置覆盖 + 当前全局默认 ----
  useEffect(() => {
    let alive = true;
    api
      .wallpaperItemPlayConfig(itemId)
      .then((r) => {
        if (!alive) return;
        setPlay(r.override ?? {});
        setGlobals(r.globals);
      })
      .catch((e) => alive && setPlayMsg(String(e)));
    return () => {
      alive = false;
    };
  }, [itemId]);

  /**
   * 改一项播放设置：立即落盘（这几项没有"输入中"的中间态，不需要防抖）。
   * 传 undefined 表示恢复「跟随全局」。
   */
  const changePlay = useCallback(
    async (patch: ItemPlayConfig) => {
      const next: ItemPlayConfig = { ...play, ...patch };
      // undefined 要真的从对象里删掉：留着 key 会被序列化成 null，
      // 后端 Option<T> 反序列化成 Some(null) 而不是 None，"跟随全局"就失效了
      for (const k of Object.keys(patch) as (keyof ItemPlayConfig)[]) {
        if (patch[k] === undefined) delete next[k];
      }
      setPlay(next);
      setPlayMsg("");
      try {
        await api.wallpaperItemPlayConfigSet(itemId, next);
      } catch (e) {
        setPlayMsg(String(e));
      }
    },
    [itemId, play],
  );

  const playOverrideCount = useMemo(
    () => (Object.keys(play) as (keyof ItemPlayConfig)[]).filter((k) => play[k] !== undefined).length,
    [play],
  );

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
    <>
      <div
        className={`props-embedded flex w-full flex-col overflow-hidden ${
          embedded
            ? "h-screen props-tint"
            : "card h-[78vh] max-w-2xl"
        }`}
        onClick={(e) => e.stopPropagation()}
      >
        {/* 标题栏（单行：图标 + 「壁纸配置」+ 壁纸名，超长截断）。
            独立窗口（embedded）：红绿灯在左上（Overlay 标题栏），标题行用 pt-9
            整行放在红绿灯正下方，不再 pl-20 让位（旧的双行+缩进太丑）；
            拖动走系统背景拖动（movableByWindowBackground，见 props_window.rs），
            红绿灯已是关闭入口。主窗口弹窗：保留 ×，配合遮罩点击与底栏完成。 */}
        <div
          {...(embedded
            ? {
                onDoubleClick: () => void getCurrentWindow().toggleMaximize(),
              }
            : {})}
          className={`flex shrink-0 items-center justify-between gap-3 border-b border-[var(--separator)] ${
            embedded ? "px-5 pb-3 pt-9" : "px-5 py-3"
          }`}
        >
          <div className="flex min-w-0 items-baseline gap-2">
            <span className="flex shrink-0 items-center gap-2 text-[14px] font-semibold">
              <IconSliders size={15} />
              壁纸配置
            </span>
            <span className="truncate text-[11.5px] text-[var(--text-2)]" title={title}>
              {title}
            </span>
          </div>
          {!embedded && (
            <button
              className="shrink-0 rounded-lg px-2 py-0.5 text-[18px] leading-none text-[var(--text-2)] hover:bg-black/5 dark:hover:bg-white/10"
              onClick={close}
              aria-label="关闭"
            >
              ×
            </button>
          )}
        </div>

        {/* 分区切换。作者属性为空时依然要能进播放设置 —— 那是这张壁纸唯一可配的东西，
            旧版在这种情况下直接显示空态、整个弹窗没有任何可操作项 */}
        <div className="flex shrink-0 items-center gap-1 border-b border-[var(--separator)] px-5 py-2">
          {(
            [
              ["props", "作者属性", defs === null ? null : all.length],
              ["play", "播放设置", playOverrideCount || null],
            ] as const
          ).map(([key, label, count]) => (
            <button
              key={key}
              className={`rounded-lg px-2.5 py-1 text-[12.5px] ${
                tab === key
                  ? "bg-[var(--accent)] text-white"
                  : "text-[var(--text-2)] hover:bg-black/5 dark:hover:bg-white/10"
              }`}
              onClick={() => setTab(key)}
            >
              {label}
              {count != null && count > 0 && (
                <span className={`ml-1 ${tab === key ? "opacity-80" : "opacity-60"}`}>{count}</span>
              )}
            </button>
          ))}
        </div>

        {tab === "play" ? (
          <PlayConfigPanel
            play={play}
            globals={globals}
            errMsg={playMsg}
            onChange={changePlay}
            onResetAll={() => void changePlay({ fit: undefined, renderDpr: undefined, sceneFps: undefined, volume: undefined })}
            overrideCount={playOverrideCount}
          />
        ) : loadError ? (
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
          <div className="flex flex-1 items-center justify-center">
            <EmptyState
              art="props"
              title="该壁纸没有作者定义的配置项"
              hint="project.json 未声明 general.properties；「播放设置」仍可调整"
            />
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
                <EmptyState art="search" title={`没有匹配「${query}」的属性`} />
              ) : (
                <div className="flex flex-col gap-3">
                  {shown.map((p) => (
                    <PropRow
                      key={p.name}
                      p={p}
                      draft={draft}
                      onChange={change}
                      onFilePick={pickFile}
                      fileUrl={fileUrl}
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
                {/* 独立窗口的关闭走红绿灯，这里的完成是冗余 */}
                {!embedded && (
                  <button className="btn btn-primary !py-1 !text-[12px]" onClick={close}>
                    完成
                  </button>
                )}
              </div>
            </div>
          </>
        )}
      </div>

      {confirmReset && (
        <ConfirmModal
          title="恢复默认"
          message={`将清除该壁纸的全部作者属性覆盖（${modifiedCount} 项），恢复 project.json 默认值。`}
          confirmText="恢复"
          onCancel={() => setConfirmReset(false)}
          onConfirm={resetAll}
        />
      )}
    </>
  );
}

/**
 * 主窗口内的配置弹窗：Portal 到 body + 居中遮罩。
 * 详情页/库内入口用它；托盘走独立窗口（props-main.tsx），不用这个。
 */
export function WallpaperPropsModal(props: {
  itemId: string;
  title: string;
  onClose: () => void;
}) {
  return createPortal(
    <div
      className="fixed inset-0 z-[80] flex items-center justify-center bg-black/50 p-8"
      onClick={props.onClose}
    >
      <WallpaperPropsPanel {...props} />
    </div>,
    document.body,
  );
}

// ---------- 单个属性行 ----------

function PropRow({
  p,
  draft,
  onChange,
  onFilePick,
  fileUrl,
}: {
  p: WebPropDef;
  draft: WebPropValues;
  onChange: (name: string, v: WebPropValues[string]) => void;
  onFilePick: (p: WebPropDef) => Promise<void>;
  /** file 属性值 → 可访问 URL（用于图片缩略图） */
  fileUrl: (val: unknown) => string;
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
            {/* 图片类 file 属性显示缩略图（WE 编辑器就是这样：背景图/LED 背景
                等属性在选择框旁给出当前图预览）。video/audio 没有可渲染的首帧，
                仍只显示文件名。URL 拼不出（内容服务器未就绪）时退化为文件名。 */}
            {p.fileType === "image" && cur ? (
              <FileThumb url={fileUrl(cur)} />
            ) : null}
            <span
              className="min-w-0 flex-1 truncate text-[11.5px] text-[var(--text-2)]"
              title={cur ? String(cur) : undefined}
            >
              {cur ? shortFileName(String(cur)) : "未设置"}
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

// ---------- 播放设置分区（每壁纸覆盖，未设置则跟随全局） ----------

/** 清晰度档位：与设置页、Rust 的 RENDER_DPR_MIN/MAX 保持一致 */
const DPR_TIERS: Array<{ v: number; label: string }> = [
  { v: 0.8, label: "省电" },
  { v: 1, label: "标准" },
  { v: 2, label: "高清" },
];
const FIT_LABELS: Record<string, string> = {
  cover: "填充",
  contain: "适应",
  stretch: "拉伸",
};
const FPS_TIERS = [30, 60, 120];

function PlayConfigPanel({
  play,
  globals,
  errMsg,
  onChange,
  onResetAll,
  overrideCount,
}: {
  play: ItemPlayConfig;
  globals: PlayConfigGlobals | null;
  errMsg: string;
  onChange: (patch: ItemPlayConfig) => void;
  onResetAll: () => void;
  overrideCount: number;
}) {
  if (!globals) {
    return (
      <div className="flex flex-1 items-center justify-center text-[13px] text-[var(--text-2)]">
        {errMsg ? <span className="text-red-500">读取失败：{errMsg}</span> : "正在读取…"}
      </div>
    );
  }
  return (
    <>
      <div className="flex-1 overflow-y-auto px-5 py-3">
        <div className="mb-3 text-[11.5px] leading-relaxed text-[var(--text-2)]">
          这些设置只作用于本张壁纸，切换壁纸后各自保留。选「跟随全局」则使用
          设置 → 通用里的值。
        </div>
        <div className="flex flex-col gap-3">
          <PlayRow
            label="显示模式"
            desc="画面与屏幕比例不一致时如何填充"
            isOverride={play.fit !== undefined}
            onFollow={() => onChange({ fit: undefined })}
          >
            <select
              className={playSelectCls}
              value={play.fit ?? ""}
              onChange={(e) =>
                onChange({ fit: (e.target.value || undefined) as ItemPlayConfig["fit"] })
              }
            >
              <option value="">跟随全局（{FIT_LABELS[globals.fit] ?? globals.fit}）</option>
              {Object.entries(FIT_LABELS).map(([v, label]) => (
                <option key={v} value={v}>
                  {label}
                </option>
              ))}
            </select>
          </PlayRow>

          <PlayRow
            label="清晰度"
            desc="越高越清晰，显存占用也越高；实际生效值不超过屏幕像素比"
            isOverride={play.renderDpr !== undefined}
            onFollow={() => onChange({ renderDpr: undefined })}
          >
            <select
              className={playSelectCls}
              value={play.renderDpr ?? ""}
              onChange={(e) =>
                onChange({ renderDpr: e.target.value ? Number(e.target.value) : undefined })
              }
            >
              <option value="">
                跟随全局（{DPR_TIERS.find((t) => t.v === globals.renderDpr)?.label ?? globals.renderDpr}）
              </option>
              {DPR_TIERS.map((t) => (
                <option key={t.v} value={t.v}>
                  {t.label}
                </option>
              ))}
            </select>
          </PlayRow>

          <PlayRow
            label="帧率限制"
            desc="越低 GPU 占用越低"
            isOverride={play.sceneFps !== undefined}
            onFollow={() => onChange({ sceneFps: undefined })}
          >
            <select
              className={playSelectCls}
              value={play.sceneFps ?? ""}
              onChange={(e) =>
                onChange({ sceneFps: e.target.value ? Number(e.target.value) : undefined })
              }
            >
              <option value="">跟随全局（{globals.sceneFps} FPS）</option>
              {FPS_TIERS.map((f) => (
                <option key={f} value={f}>
                  {f} FPS
                </option>
              ))}
            </select>
          </PlayRow>

          <PlayRow
            label="音量"
            desc="壁纸自带音频的音量；0 即静音。默认静音，避免多张壁纸同时出声"
            isOverride={play.volume !== undefined}
            onFollow={() => onChange({ volume: undefined })}
          >
            <div className="flex items-center gap-2">
              <input
                type="range"
                min={0}
                max={1}
                step={0.05}
                value={play.volume ?? 0}
                onChange={(e) => onChange({ volume: Number(e.target.value) })}
                className="w-28 accent-[var(--accent)]"
              />
              <span className="w-9 text-right text-[12px] tabular-nums text-[var(--text-2)]">
                {Math.round((play.volume ?? 0) * 100)}%
              </span>
            </div>
          </PlayRow>
        </div>
      </div>

      <div className="flex shrink-0 items-center gap-3 border-t border-[var(--separator)] px-5 py-3">
        <span className={`truncate text-[12px] ${errMsg ? "text-red-500" : "text-[var(--text-2)]"}`}>
          {errMsg
            ? `保存失败：${errMsg}`
            : overrideCount > 0
              ? `${overrideCount} 项为本壁纸专属`
              : "全部跟随全局设置"}
        </span>
        <button
          className="btn ml-auto !py-1 !text-[12px]"
          disabled={overrideCount === 0}
          onClick={onResetAll}
        >
          ↺ 全部跟随全局
        </button>
      </div>
    </>
  );
}

const playSelectCls =
  "rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1.5 text-[12.5px] focus:outline-none focus:ring-1 focus:ring-[var(--accent)]";

function PlayRow({
  label,
  desc,
  isOverride,
  onFollow,
  children,
}: {
  label: string;
  desc: string;
  isOverride: boolean;
  onFollow: () => void;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-start justify-between gap-4 border-t border-[var(--separator)] pt-2.5 first:border-t-0 first:pt-0">
      <div className="min-w-0">
        <div className="flex items-center gap-1.5 text-[13px] font-medium">
          {label}
          {/* 标出哪些项被本壁纸覆盖了：不标的话用户分不清当前值是自己设的还是全局带来的 */}
          {isOverride && (
            <button
              className="rounded bg-[var(--accent)]/15 px-1.5 text-[10px] text-[var(--accent)] hover:bg-[var(--accent)]/25"
              onClick={onFollow}
              title="点击恢复为跟随全局"
            >
              专属 ×
            </button>
          )}
        </div>
        <div className="mt-0.5 text-[11.5px] leading-relaxed text-[var(--text-2)]">{desc}</div>
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

/** file 属性的文件名简写：wire 值常带目录前缀（web/img/a.png / we-props/x.png），
 *  全路径会把值列撑爆，只显示末段，完整路径放 title */
function shortFileName(v: string): string {
  const clean = v.split(/[?#]/)[0].replace(/\\/g, "/");
  const i = clean.lastIndexOf("/");
  return i >= 0 ? clean.slice(i + 1) : clean;
}

/** 图片类 file 属性的缩略图。加载失败（文件丢失/非图片）静默隐藏，不占布局 */
function FileThumb({ url }: { url: string }) {
  const [failed, setFailed] = useState(false);
  const [loaded, setLoaded] = useState(false);
  if (!url || failed) return null;
  return (
    <img
      src={url}
      alt=""
      onLoad={() => setLoaded(true)}
      onError={() => setFailed(true)}
      className={`h-9 w-12 shrink-0 rounded border border-[var(--separator)] object-cover transition-opacity ${
        loaded ? "opacity-100" : "opacity-0"
      }`}
    />
  );
}
