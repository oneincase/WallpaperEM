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
import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  api,
  type AuthorSummary,
  type ItemPlayConfig,
  type PlayConfigGlobals,
  type PropMedia,
  type WebPropDef,
  type WebPropValues,
} from "../api/steam";
import { evalCondition } from "../lib/weCondition";
import {
  PARTICLE_STOPS,
  POST_STOPS,
  PRESET_LABELS,
  QUALITY_PRESETS,
  deriveQualityPreset,
  stopLabel,
} from "../lib/qualityPresets";
import { refreshItemPropsCache } from "../hooks/useItemProps";
import { ConfirmModal } from "./ConfirmModal";
import { tr, trMsg } from "../lib/i18n";
import { EmptyState } from "./EmptyState";
import { WindowControls } from "./WindowControls";
import { useOs } from "../lib/platform";
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

/** WE 属性窗口里有对应控件的属性类型 */
const KNOWN_PROP_TYPES = new Set([
  "color",
  "bool",
  "slider",
  "combo",
  "textinput",
  "file",
  "directory",
  "text",
  "group",
]);

/** 该属性是否会在 WE 属性窗口里占一行（可编辑控件 / 非空分节标题 / 带图横幅） */
function isDisplayableProp(p: WebPropDef): boolean {
  if (!KNOWN_PROP_TYPES.has(p.ptype)) return false;
  if (p.ptype === "text" || p.ptype === "group")
    return !!p.text.trim() || !!p.media?.length;
  return true;
}

const IMAGE_FILE_EXT = /\.(png|jpe?g|webp|gif|bmp|avif|svg)$/i;

/** 图片类 file 属性判定：project.json 的 fileType 常常不标（真实语料 50 个
 *  file 属性只有 2 个标了 image），WE 按文件扩展名给预览，这里同样按扩展名兜底 */
function isImageProp(p: WebPropDef, cur: unknown): boolean {
  if (!cur) return false;
  if (p.fileType === "image") return true;
  if (p.fileType) return false; // 明确声明 video/audio 的不显示图
  return IMAGE_FILE_EXT.test(String(cur).split(/[?#]/)[0]);
}

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
  /** group 分节的开合状态（组名 → 是否展开；缺省展开） */
  const [groupOpen, setGroupOpen] = useState<Record<string, boolean>>({});
  /** 当前分区。作者属性为空的壁纸直接落到播放设置（那才是它唯一能配的东西） */
  const [tab, setTab] = useState<"props" | "play">("props");
  /** 每壁纸播放设置覆盖（字段缺失 = 跟随全局）+ 当前全局默认 */
  const [play, setPlay] = useState<ItemPlayConfig>({});
  const [globals, setGlobals] = useState<PlayConfigGlobals | null>(null);
  const [playMsg, setPlayMsg] = useState("");
  /** 独立窗口（embedded）的窗口壳按平台分叉：macOS 背景拖动 + ×，Win/Linux 拖拽区 + 自绘控制 */
  const isMac = useOs() === "macos";

  const draftRef = useRef(draft);
  draftRef.current = draft;
  const defsRef = useRef<WebPropDef[] | null>(null);
  defsRef.current = defs;
  const saveTimer = useRef<number | null>(null);

  // ---- 独立窗口拖动：macOS 走系统背景拖动（movableByWindowBackground，见
  // props_window.rs）——JS 侧不要再插手手动 setPosition，WKWebView 在非交互
  // 区域会吃掉鼠标事件走原生手势，两套并存就是"时灵时不灵"。Windows/Linux
  // 没有背景拖动，头部挂 data-tauri-drag-region 交给系统拖窗（下方标题栏）。----

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
      // Windows 作者写的反斜杠统一成正斜杠；剥掉 query/hash（本地文件用不到），
      // 再按段编码 —— 中文/空格文件名不编码会让 <img> 请求直接 404
      const clean = rel.split(/[?#]/)[0].replace(/\\/g, "/").replace(/^\/+/, "");
      return fileBase + encodeURI(clean);
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
   * 改一项播放设置。本地状态立即跟上（滑条拖动要实时），落盘去抖 350ms 只发
   * 最后一次 —— 滑条是连续 onChange，逐次落盘会连着重载壁纸窗口（贴图/法线
   * 倍率是挂载期参数，改动即重载）。
   * 传 undefined 表示恢复「跟随全局」。
   */
  const playCommitTimer = useRef<number | null>(null);
  const playPending = useRef<ItemPlayConfig | null>(null);
  const commitPlay = useCallback(
    async (cfg: ItemPlayConfig) => {
      try {
        await api.wallpaperItemPlayConfigSet(itemId, cfg);
      } catch (e) {
        setPlayMsg(String(e));
      }
    },
    [itemId],
  );
  const changePlay = useCallback(
    (patch: ItemPlayConfig) => {
      const next: ItemPlayConfig = { ...play, ...patch };
      // undefined 要真的从对象里删掉：留着 key 会被序列化成 null，
      // 后端 Option<T> 反序列化成 Some(null) 而不是 None，"跟随全局"就失效了
      for (const k of Object.keys(patch) as (keyof ItemPlayConfig)[]) {
        if (patch[k] === undefined) delete next[k];
      }
      setPlay(next);
      setPlayMsg("");
      playPending.current = next;
      if (playCommitTimer.current) window.clearTimeout(playCommitTimer.current);
      playCommitTimer.current = window.setTimeout(() => {
        playCommitTimer.current = null;
        const cfg = playPending.current;
        playPending.current = null;
        if (cfg) void commitPlay(cfg);
      }, 350);
    },
    // commitPlay 身份稳定（只依赖 itemId）；play 走 playPending 不进依赖，
    // 否则每次 setState 换闭包，去抖永远等不到触发
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [itemId],
  );

  /** 立即落盘去抖中的播放设置（关闭/卸载时冲刷，防止拖完就关丢改动） */
  const flushPlay = useCallback(async () => {
    if (playCommitTimer.current) {
      window.clearTimeout(playCommitTimer.current);
      playCommitTimer.current = null;
    }
    if (playPending.current) {
      const cfg = playPending.current;
      playPending.current = null;
      await commitPlay(cfg);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [itemId]);

  /** 画质档位一键套用：六个画质参数整体写成本壁纸专属值（抗锯齿恒 off 不动） */
  const applyPlayPreset = useCallback(
    (id: "low" | "medium" | "high") => {
      const p = QUALITY_PRESETS[id];
      changePlay({
        renderDpr: p.renderDpr,
        sceneFps: p.sceneFps,
        particles: p.particles,
        postProcessing: p.post,
        resources: p.resources,
        resourcesNormal: p.resourcesNormal,
      });
    },
    [changePlay],
  );

  /** 画质档位行「专属 ×」：六个画质参数全部回到跟随全局 */
  const followPlayQuality = useCallback(() => {
    changePlay({
      renderDpr: undefined,
      sceneFps: undefined,
      particles: undefined,
      postProcessing: undefined,
      resources: undefined,
      resourcesNormal: undefined,
    });
  }, [changePlay]);

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
      void flushPlay();
    };
  }, [doSave, flushPlay]);

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
    void flushPlay();
    onClose();
  }, [doSave, flushPlay, onClose]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !confirmReset) close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [close, confirmReset]);

  // ---- 派生：分节 → condition 显隐（按当前草稿求值，开关联动即时反映）→ 搜索过滤 ----
  // 只统计/渲染 WE 属性窗口真正会显示的项（与 WE 对齐，不多也不少）：
  // - scenetexture / usershortcut 等 scene 内部类型没有对应控件，WE 不显示
  // - 空 text 是作者留的空白间隔（kong10/fengexian3 这类间隔键很常见），
  //   WE 显示为一小段空白，不占列表项、不参与计数与搜索
  // 注意保存仍遍历完整 defs（doSave 用 defsRef），隐藏类型的已有覆盖不会丢
  const all = useMemo(() => (defs ?? []).filter(isDisplayableProp), [defs]);

  // 按 WE 语义把 type=group 当可折叠分节：其后属性归属该组，直到下一个 group
  const sections = useMemo(() => {
    const out: { group: WebPropDef | null; items: WebPropDef[] }[] = [{ group: null, items: [] }];
    for (const p of all) {
      if (p.ptype === "group") {
        out.push({ group: p, items: [] });
      } else {
        out[out.length - 1].items.push(p);
      }
    }
    return out;
  }, [all]);

  // condition 显隐：组条件作用于整节，条目条件作用于自身
  const visibleSections = useMemo(
    () =>
      sections
        .filter((sec) => !sec.group || evalCondition(sec.group.condition, draft))
        .map((sec) => ({
          ...sec,
          items: sec.items.filter((p) => evalCondition(p.condition, draft)),
        })),
    [sections, draft],
  );
  const visibleCount = useMemo(
    () => visibleSections.reduce((n, s) => n + s.items.length + (s.group ? 1 : 0), 0),
    [visibleSections],
  );

  // 搜索：组名命中整组保留，否则按条目过滤；无搜索时只剩标题的空组也显示（作者分隔）
  const q = query.trim().toLowerCase();
  const shownSections = useMemo(() => {
    if (!q) return visibleSections;
    const hit = (p: WebPropDef) =>
      propLabel(p).toLowerCase().includes(q) || p.name.toLowerCase().includes(q);
    return visibleSections
      .map((sec) =>
        sec.group && hit(sec.group)
          ? sec
          : { group: sec.group, items: sec.items.filter(hit) },
      )
      .filter((sec) => sec.items.length > 0);
  }, [visibleSections, q]);
  const shownCount = useMemo(
    () => shownSections.reduce((n, s) => n + s.items.length + (s.group ? 1 : 0), 0),
    [shownSections],
  );

  const modifiedCount = useMemo(
    () => all.filter((p) => p.value !== null && !sameValue(draft[p.name], p.default)).length,
    [all, draft]
  );

  const showSearch = all.length > 6;
  const searchHint = q ? tr("匹配 {n} 项", { n: shownCount }) : "";
  const infoBits = [
    tr("共 {n} 项", { n: all.length }),
    modifiedCount > 0 ? tr("已修改 {n} 项", { n: modifiedCount }) : "",
    visibleCount < all.length
      ? tr("{n} 项因条件隐藏", { n: all.length - visibleCount })
      : "",
  ].filter(Boolean);

  return (
    <>
      <div
        className={`props-embedded flex w-full flex-col overflow-hidden ${
          embedded
            ? "h-full"
            : "card glass-panel animate-modal-pop h-[78vh] max-w-2xl"
        }`}
        onClick={(e) => e.stopPropagation()}
      >
        {/* 标题栏（单行：图标 + 「壁纸配置」+ 壁纸名，超长截断）。
            独立窗口（embedded）全平台无边框（decorations:false，见 props_window.rs）：
            macOS 拖动走系统背景拖动（movableByWindowBackground），双击头部最大化、
            右侧 × 关闭；Windows/Linux 没有背景拖动，头部挂 data-tauri-drag-region
            交给系统拖窗（连带双击最大化由 Tauri 内建处理 —— 这里绝不能再挂
            onDoubleClick，两次 toggle 会相互抵消），右侧换成自绘 WindowControls
            （min/max/close）。主窗口弹窗：保留 ×，配合遮罩点击与底栏完成。 */}
        <div
          {...(embedded && isMac
            ? {
                onDoubleClick: () => void getCurrentWindow().toggleMaximize(),
              }
            : {})}
          data-tauri-drag-region={embedded && !isMac ? "deep" : undefined}
          className={`flex shrink-0 items-center justify-between gap-3 border-b border-[var(--separator)] px-5 ${
            // 独立窗口头部定高：WindowControls 是 h-full 的通栏按钮（同原生标题条
            // 的整条 hover），高度得有确定值可依；弹窗保持自适应行高
            embedded ? "h-12" : "py-3"
          }`}
        >
          <div className="flex min-w-0 items-baseline gap-2">
            <span className="flex shrink-0 items-center gap-2 text-[14px] font-semibold">
              <IconSliders size={15} />
              {tr("壁纸配置")}
            </span>
            <span className="truncate text-[11.5px] text-[var(--text-2)]" title={title}>
              {title}
            </span>
          </div>
          {embedded && !isMac ? (
            <WindowControls />
          ) : (
            <button
              className="shrink-0 rounded-lg px-2 py-0.5 text-[18px] leading-none text-[var(--text-2)] hover:bg-white/10"
              onClick={close}
              aria-label={tr("关闭")}
            >
              ×
            </button>
          )}
        </div>

        {/* 作者行（WE 式：头像 + 昵称 + 放大镜浏览作者工坊页）。
            本地未发布/解析失败时整行隐藏 —— WE 对无工坊关联的壁纸也不显示作者 */}
        <AuthorRow itemId={itemId} />

        {/* 分区切换。作者属性为空时依然要能进播放设置 —— 那是这张壁纸唯一可配的东西，
            旧版在这种情况下直接显示空态、整个弹窗没有任何可操作项 */}
        <div className="flex shrink-0 items-center gap-1 border-b border-[var(--separator)] px-5 py-2">
          {(
            [
              ["props", tr("作者属性"), defs === null ? null : all.length],
              ["play", tr("播放设置"), playOverrideCount || null],
            ] as const
          ).map(([key, label, count]) => (
            <button
              key={key}
              className={`rounded-lg px-2.5 py-1 text-[12.5px] ${
                tab === key
                  ? "border border-[var(--accent-strong)] bg-[var(--accent)] text-[var(--accent-fg)]"
                  : "text-[var(--text-2)] hover:bg-white/10"
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
            onResetAll={() =>
              changePlay({
                fit: undefined,
                renderDpr: undefined,
                sceneFps: undefined,
                volume: undefined,
                aa: undefined,
                particles: undefined,
                postProcessing: undefined,
                resources: undefined,
                resourcesNormal: undefined,
              })
            }
            onApplyPreset={applyPlayPreset}
            onFollowQuality={followPlayQuality}
            overrideCount={playOverrideCount}
          />
        ) : loadError ? (
          <div className="flex flex-1 flex-col items-center justify-center gap-3 px-6 text-center">
            <div className="text-[13px] text-red-500">
              {tr("读取失败")}: {trMsg(loadError)}
            </div>
            <button className="btn" onClick={close}>
              {tr("关闭")}
            </button>
          </div>
        ) : defs === null ? (
          <div className="flex flex-1 items-center justify-center text-[13px] text-[var(--text-2)]">
            {tr("正在读取 project.json…")}
          </div>
        ) : all.length === 0 ? (
          <div className="flex flex-1 items-center justify-center">
            <EmptyState
              art="props"
              title={tr("该壁纸没有作者定义的配置项")}
              hint={tr("project.json 未声明 general.properties；「播放设置」仍可调整")}
            />
          </div>
        ) : (
          <>
            {/* 工具条：搜索 + 统计 */}
            <div className="flex shrink-0 items-center gap-3 border-b border-[var(--separator)] px-5 py-2">
              {showSearch && (
                <input
                  type="text"
                  className="w-52 rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1 text-[12px] placeholder:text-[var(--text-2)]/60 focus:outline-none focus:ring-1 focus:ring-[var(--accent-strong)]"
                  placeholder={tr("搜索属性…")}
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                />
              )}
              <div className="ml-auto flex items-center gap-2 text-[11.5px] text-[var(--text-2)]">
                {searchHint && <span>{searchHint} ·</span>}
                <span className="truncate">{infoBits.join(" · ")}</span>
              </div>
            </div>

            {/* 属性列表（group 为可折叠分节；节内条目跟随组开合） */}
            <div className="flex-1 overflow-y-auto px-5 py-3">
              {shownCount === 0 ? (
                <EmptyState art="search" title={tr("没有匹配「{query}」的属性", { query })} />
              ) : (
                <div className="flex flex-col gap-3">
                  {shownSections.map((sec) =>
                    sec.group ? (
                      <GroupSection
                        key={sec.group.name}
                        group={sec.group}
                        open={groupOpen[sec.group.name] ?? true}
                        onToggle={() =>
                          setGroupOpen((m) => ({
                            ...m,
                            [sec.group!.name]: !(m[sec.group!.name] ?? true),
                          }))
                        }
                        fileUrl={fileUrl}
                      >
                        {sec.items.map((p) => (
                          <PropRow
                            key={p.name}
                            p={p}
                            draft={draft}
                            onChange={change}
                            onFilePick={pickFile}
                            fileUrl={fileUrl}
                          />
                        ))}
                      </GroupSection>
                    ) : (
                      sec.items.map((p) => (
                        <PropRow
                          key={p.name}
                          p={p}
                          draft={draft}
                          onChange={change}
                          onFilePick={pickFile}
                          fileUrl={fileUrl}
                        />
                      ))
                    ),
                  )}
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
                  ? `${tr("保存失败")}: ${trMsg(errMsg)}`
                  : saveState === "saving"
                    ? tr("正在保存…")
                    : saveState === "pending"
                      ? tr("更改即将自动生效…")
                      : saveState === "saved"
                        ? tr("✓ 已保存并实时生效")
                        : ""}
              </span>
              <div className="ml-auto flex items-center gap-2">
                <button
                  className="btn !py-1 !text-[12px]"
                  disabled={modifiedCount === 0}
                  onClick={() => setConfirmReset(true)}
                >
                  ↺ {tr("恢复默认")}
                </button>
                {/* 独立窗口的关闭走红绿灯，这里的完成是冗余 */}
                {!embedded && (
                  <button className="btn btn-primary !py-1 !text-[12px]" onClick={close}>
                    {tr("完成")}
                  </button>
                )}
              </div>
            </div>
          </>
        )}
      </div>

      {confirmReset && (
        <ConfirmModal
          title={tr("恢复默认")}
          message={tr("将清除该壁纸的全部作者属性覆盖（{n} 项），恢复 project.json 默认值。", {
            n: modifiedCount,
          })}
          confirmText={tr("恢复")}
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
      className="animate-overlay fixed inset-0 z-[80] flex items-center justify-center bg-black/25 p-8"
      onClick={props.onClose}
    >
      <WallpaperPropsPanel {...props} />
    </div>,
    document.body,
  );
}

// ---------- 作者行（WE：头像 + 昵称 + 放大镜按作者浏览） ----------

function AuthorRow({ itemId }: { itemId: string }) {
  // undefined = 解析中（显示骨架，避免解析完成后面板高度跳动）
  const [author, setAuthor] = useState<AuthorSummary | null | undefined>(undefined);
  const [avatarFailed, setAvatarFailed] = useState(false);
  useEffect(() => {
    let alive = true;
    setAuthor(undefined);
    setAvatarFailed(false);
    api
      .libraryItemAuthor(itemId)
      .then((a) => alive && setAuthor(a))
      .catch(() => alive && setAuthor(null));
    return () => {
      alive = false;
    };
  }, [itemId]);

  if (author === undefined) {
    return (
      <div className="flex shrink-0 items-center gap-2 border-b border-[var(--separator)] px-5 py-2">
        <span className="h-[18px] w-[18px] animate-pulse rounded-full bg-[var(--separator)]" />
        <span className="h-2.5 w-24 animate-pulse rounded bg-[var(--separator)]" />
      </div>
    );
  }
  if (!author) return null;

  return (
    <div className="flex shrink-0 items-center gap-2 border-b border-[var(--separator)] px-5 py-2">
      {author.avatarUrl && !avatarFailed ? (
        <img
          src={author.avatarUrl}
          alt=""
          draggable={false}
          onError={() => setAvatarFailed(true)}
          className="h-[18px] w-[18px] shrink-0 rounded-full object-cover"
        />
      ) : (
        <span className="flex h-[18px] w-[18px] shrink-0 items-center justify-center rounded-full bg-[var(--separator)] text-[10px] text-[var(--text-2)]">
          ?
        </span>
      )}
      <span className="min-w-0 flex-1 truncate text-[12px] text-[var(--text-2)]" title={author.name}>
        {tr("作者")} · {author.name}
      </span>
    </div>
  );
}

// ---------- 单个属性行（WE 几何：属性名小字在上、控件通栏在下） ----------

/**
 * WE 属性区的行骨架：标签行（属性名 + 已自定义点 + 可选右侧挂件 + 单项重置 ↺）
 * + 通栏控件。bool 是唯一的例外（左侧方框勾选与标签同行），不走这个骨架。
 */
function RowShell({
  label,
  title,
  isDefault,
  canReset,
  aside,
  onReset,
  children,
}: {
  label: string;
  title: string;
  isDefault: boolean;
  /** 有默认值可恢复（project.json 提供了 default） */
  canReset: boolean;
  aside?: React.ReactNode;
  onReset: () => void;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center gap-2">
        <div
          className="min-w-0 flex-1 whitespace-pre-line text-[12px] leading-snug text-[var(--text-2)]"
          title={title}
        >
          {label}
          {!isDefault && (
            <span className="ml-1 text-[var(--accent-strong)]" title={tr("已自定义")}>
              •
            </span>
          )}
        </div>
        {aside}
        {canReset && !isDefault && (
          <button
            className="shrink-0 text-[11px] text-[var(--text-2)] hover:text-[var(--accent-strong)]"
            title={tr("恢复该属性默认值")}
            onClick={onReset}
          >
            ↺
          </button>
        )}
      </div>
      {children}
    </div>
  );
}

// ---------- 文案图片（后端 we_props 从属性 text 抽出的 media） ----------
//
// 与上游 WebWallGL 测试台同一条管线：提取/清洗在解析层完成（we_props.rs），
// 面板只渲染结构化 media —— text 不会再残留 HTML，也不会有 imgsrchttp… 残渣键名。
// src 两种形态：http(s) 外链直接用；包内相对路径走内容服务器 /media 前缀（同
// file 属性缩略图）。

/** 作者 HTML 常写 height=30 当图标、width=2000 当横幅：横幅按比例缩，图标才钉高度
 *  （与上游测试台 applyMediaSize 同一套规则） */
function mediaStyle(m: PropMedia): React.CSSProperties {
  const w = cssLen(m.width);
  const h = cssLen(m.height);
  const hPx = h && h.endsWith("px") ? Number.parseFloat(h) : NaN;
  if (w && w.endsWith("%")) return { width: w, height: "auto" };
  if (Number.isFinite(hPx) && hPx >= 16 && hPx <= 96) return { height: h, width: "auto" };
  if (w) return { width: w, height: "auto" };
  if (h) return { height: h, width: "auto" };
  return { height: "auto" };
}

/** CSS 尺寸：105% / 120px / 120 / 1.5em；裸数字按 px */
function cssLen(v?: string): string | undefined {
  if (!v) return undefined;
  const s = v.trim().replace(/^['"]+|['"]+$/g, "");
  if (!s) return undefined;
  if (/%|px|em|rem|vh|vw$/i.test(s)) return s;
  const n = Number(s);
  return Number.isFinite(n) && n > 0 ? `${n}px` : undefined;
}

function PropMediaView({
  media,
  align = "center",
  fileUrl,
}: {
  media: PropMedia[];
  /** text 属性的分隔/赞助图居中；属性行里的示意图左对齐 */
  align?: "center" | "start";
  fileUrl: (val: unknown) => string;
}) {
  return (
    <div
      className={`my-1 flex flex-col gap-1.5 ${
        align === "center" ? "items-center" : "items-start"
      }`}
    >
      {media.map((m, i) => {
        const src = /^https?:\/\//i.test(m.src) ? m.src : fileUrl(m.src);
        if (!src) return null; // 内容服务器未就绪：就绪后重渲染自然出现
        const img = (
          <img
            src={src}
            alt=""
            loading="lazy"
            draggable={false}
            // qpic.cn 等图床校验 Referer，不带才能加载（上游测试台同样处理）
            referrerPolicy="no-referrer"
            style={mediaStyle(m)}
            className="block max-w-full rounded-[3px]"
          />
        );
        // <a> 包裹的图可点击（href 后端已限定 http(s)），走系统浏览器
        return m.href ? (
          <a
            key={i}
            href={m.href}
            className="block max-w-full"
            onClick={(e) => {
              e.preventDefault();
              void openUrl(m.href!).catch(() => {});
            }}
          >
            {img}
          </a>
        ) : (
          <React.Fragment key={i}>{img}</React.Fragment>
        );
      })}
    </div>
  );
}

/** 作者常把整段 <img> HTML 写进属性 key，WE 剥掉符号后变成 imgsrchttp… 这种「名字」。
 *  有抽出的图时不要回退显示这段残渣（与上游测试台 looksLikeHtmlResidue 一致） */
function looksLikeHtmlResidue(s: string): boolean {
  if (s.length < 24 || /\s/.test(s)) return false;
  return /^(imgsrc|ahref|hrbig|brahref)/i.test(s) || /viewer_4|photostore|qpiccn/i.test(s);
}

/** 面板显示用的属性名：优先解析后的文案；纯图属性与残渣键名不显示 */
function propLabel(p: WebPropDef): string {
  if (p.text && !looksLikeHtmlResidue(p.text)) return p.text;
  if (p.media?.length) return "";
  if (looksLikeHtmlResidue(p.name)) return "";
  return p.text || p.name;
}

/** 行 tooltip（键名 + 类型）；残渣键名只留类型 */
function propTitle(p: WebPropDef): string {
  return looksLikeHtmlResidue(p.name) ? p.ptype : `${p.name} · ${p.ptype}`;
}

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
  /** file/media 相对路径 → 可访问 URL（内容服务器） */
  fileUrl: (val: unknown) => string;
}) {
  const cur = draft[p.name];
  const isDefault = sameValue(cur, p.default);
  const controlCls =
    "w-full rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1.5 text-[12.5px] focus:outline-none focus:ring-1 focus:ring-[var(--accent-strong)]";
  const label = propLabel(p);
  const reset = () => onChange(p.name, p.default as WebPropValues[string]);
  const canReset = p.default !== null;

  // WE 的 text 是静态说明/分节标题（不可编辑）：文案图（分隔 GIF/赞助图）居中在上，
  // 说明文字在下。group 在列表层当可折叠分节处理，不会流到这里。
  // 未知类型（scenetexture/usershortcut 等 scene 专属）WE 不显示，整行跳过
  // （列表已按 isDisplayableProp 预过滤，此为防御兜底）
  if (!KNOWN_PROP_TYPES.has(p.ptype)) return null;
  if (p.ptype === "group") return null;

  if (p.ptype === "text") {
    if (!label && !p.media?.length) return <div className="h-1" aria-hidden />;
    return (
      <div className="mt-1 border-t border-[var(--separator)] pt-2.5 leading-relaxed text-[var(--text-2)]">
        {p.media?.length ? <PropMediaView media={p.media} fileUrl={fileUrl} /> : null}
        {label && (
          <div className="whitespace-pre-line text-[12px] font-medium" title={propTitle(p)}>
            {label}
          </div>
        )}
      </div>
    );
  }

  // bool：WE 是「左侧方框勾选 + 标签同行」的单行布局，没有通栏控件
  if (p.ptype === "bool") {
    return (
      <div>
        {p.media?.length ? <PropMediaView media={p.media} align="start" fileUrl={fileUrl} /> : null}
        <div className="flex items-center gap-2">
          <button
            role="checkbox"
            aria-checked={!!cur}
            className={`flex h-4 w-4 shrink-0 items-center justify-center rounded-[3px] border transition-colors ${
              cur
                ? "border-[var(--accent-strong)] bg-[var(--accent-fill)] text-[var(--accent-fg)]"
                : "border-[var(--separator)] bg-[var(--card)] hover:border-[var(--accent-strong)]"
            }`}
            onClick={() => onChange(p.name, !cur)}
          >
            {!!cur && (
              <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                <path d="M1.5 5.5l2.5 2.5 4.5-5.5" />
              </svg>
            )}
          </button>
          <div
            className="min-w-0 flex-1 cursor-pointer whitespace-pre-line text-[12.5px] leading-snug"
            title={propTitle(p)}
            onClick={() => onChange(p.name, !cur)}
          >
            {label}
            {!isDefault && (
              <span className="ml-1 text-[var(--accent-strong)]" title={tr("已自定义")}>
                •
              </span>
            )}
          </div>
          {canReset && !isDefault && (
            <button
              className="shrink-0 text-[11px] text-[var(--text-2)] hover:text-[var(--accent-strong)]"
              title={tr("恢复该属性默认值")}
              onClick={reset}
            >
              ↺
            </button>
          )}
        </div>
      </div>
    );
  }

  const controls = (
    <>
      {p.ptype === "color" && (
        <ColorControl value={cur} onChange={(v) => onChange(p.name, v)} />
      )}
      {p.ptype === "slider" && (
        <SliderTrack p={p} value={cur} onChange={(v) => onChange(p.name, v)} />
      )}
      {p.ptype === "combo" && (
        <ComboControl p={p} cur={cur} draft={draft} controlCls={controlCls} onChange={(v) => onChange(p.name, v)} />
      )}
      {p.ptype === "textinput" && (
        <input
          type="text"
          className={controlCls}
          value={String(cur ?? "")}
          onChange={(e) => onChange(p.name, e.target.value)}
        />
      )}
      {p.ptype === "file" && (
        <div className="flex items-center gap-2">
          {/* 图片类 file 属性显示缩略图（WE 编辑器就是这样：背景图/LED 背景
              等属性在选择框旁给出当前图预览）。video/audio 没有可渲染的首帧，
              仍只显示文件名。URL 拼不出（内容服务器未就绪）时退化为文件名。 */}
          {isImageProp(p, cur) ? <FileThumb url={fileUrl(cur)} /> : null}
          <span
            className="min-w-0 flex-1 truncate text-[11.5px] text-[var(--text-2)]"
            title={cur ? String(cur) : undefined}
          >
            {cur ? shortFileName(String(cur)) : tr("未设置")}
          </span>
          <button className="btn !py-1 !text-[11.5px]" onClick={() => void onFilePick(p)}>
            {tr("选择文件…")}
          </button>
        </div>
      )}
      {/* directory：选择本地目录，存绝对路径（WE 语义）；不进壁纸站点目录故不补前缀 */}
      {p.ptype === "directory" && (
        <div className="flex items-center gap-2">
          <span
            className="min-w-0 flex-1 truncate text-[11.5px] text-[var(--text-2)]"
            title={cur ? String(cur) : undefined}
          >
            {cur ? String(cur) : tr("未设置")}
          </span>
          <button className="btn !py-1 !text-[11.5px]" onClick={() => void onFilePick(p)}>
            {tr("选择目录…")}
          </button>
        </div>
      )}
    </>
  );

  return (
    <div>
      {p.media?.length ? <PropMediaView media={p.media} align="start" fileUrl={fileUrl} /> : null}
      {label ? (
        <RowShell
          label={label}
          title={propTitle(p)}
          isDefault={isDefault}
          canReset={canReset}
          onReset={reset}
          // WE 把滑条当前值放在标签行右端
          aside={
            p.ptype === "slider" ? (
              <SliderValueInput p={p} value={cur} onChange={(v) => onChange(p.name, v)} />
            ) : undefined
          }
        >
          {controls}
        </RowShell>
      ) : (
        // 纯图属性（标签为残渣/空）：图后面直接跟控件
        controls
      )}
    </div>
  );
}

// ---------- group：可折叠分节（WE 语义：其后属性归属该组，直到下一个 group） ----------

function GroupSection({
  group,
  open,
  onToggle,
  fileUrl,
  children,
}: {
  group: WebPropDef;
  open: boolean;
  onToggle: () => void;
  fileUrl: (val: unknown) => string;
  children: React.ReactNode;
}) {
  // 空 text 的组没有标签可显示（极少见；spacer 多为 text 而非 group），回退键名
  const label = propLabel(group) || group.name;
  return (
    <section className="flex flex-col">
      <button
        type="button"
        className="flex w-full items-center gap-1.5 border-t border-[var(--separator)] pb-0.5 pt-2.5 text-left"
        onClick={onToggle}
        aria-expanded={open}
        title={propTitle(group)}
      >
        <svg
          width="9"
          height="9"
          viewBox="0 0 10 10"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.6"
          strokeLinecap="round"
          strokeLinejoin="round"
          className={`shrink-0 text-[var(--text-2)] transition-transform ${open ? "" : "-rotate-90"}`}
        >
          <path d="M2 3.5l3 3 3-3" />
        </svg>
        <span className="min-w-0 flex-1 whitespace-pre-line text-[12.5px] font-semibold text-[var(--text-1)]">
          {label}
        </span>
      </button>
      {group.media?.length ? <PropMediaView media={group.media} fileUrl={fileUrl} /> : null}
      {open && <div className="mt-3 flex flex-col gap-3">{children}</div>}
    </section>
  );
}

// ---------- color：通栏色块按钮 + 行内展开的 RGB 取色器（WE 式） ----------

function ColorControl({
  value,
  onChange,
}: {
  value: WebPropValues[string] | undefined;
  onChange: (v: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const hex = rgbStrToHex(String(value ?? "0 0 0"));
  const n = parseInt(hex.slice(1), 16);
  const chans = [(n >> 16) & 255, (n >> 8) & 255, n & 255];
  // 色块上的文字颜色按亮度取黑/白，保证任何底色都可读
  const luminance = 0.299 * chans[0] + 0.587 * chans[1] + 0.114 * chans[2];
  const onSwatch = luminance > 150 ? "text-black/70" : "text-white/90";

  const setChannel = (i: number, v: number) => {
    const next = chans.slice();
    next[i] = v;
    const wire = hexToRgbStr(`#${next.map((c) => c.toString(16).padStart(2, "0")).join("")}`);
    if (wire !== null) onChange(wire);
  };

  return (
    <div>
      <button
        type="button"
        className="flex h-7 w-full items-center justify-between rounded-[4px] border border-black/20 px-2 transition-shadow hover:ring-1 hover:ring-[var(--accent-strong)]"
        style={{ backgroundColor: hex }}
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
      >
        <span className={`text-[11px] font-medium uppercase tracking-wide ${onSwatch}`}>{hex}</span>
        <span className={`text-[10px] ${onSwatch} opacity-70`}>{open ? "▴" : "▾"}</span>
      </button>
      {open && (
        <div className="mt-1.5 flex flex-col gap-1.5 rounded-md border border-[var(--separator)] bg-[var(--card)] p-2.5">
          {(["R", "G", "B"] as const).map((label, i) => (
            <div key={label} className="flex items-center gap-2">
              <span className="w-3 shrink-0 text-[11px] text-[var(--text-2)]">{label}</span>
              <input
                type="range"
                min={0}
                max={255}
                step={1}
                value={chans[i]}
                onChange={(e) => setChannel(i, Number(e.target.value))}
                className="min-w-0 flex-1 accent-[var(--accent-strong)]"
              />
              <span className="w-7 shrink-0 text-right text-[11px] tabular-nums text-[var(--text-2)]">
                {chans[i]}
              </span>
            </div>
          ))}
          <div className="flex items-center gap-2 pt-0.5">
            <span className="w-3 shrink-0 text-[11px] text-[var(--text-2)]">#</span>
            <HexInput
              hex={hex}
              onCommit={(h) => {
                const wire = hexToRgbStr(h);
                if (wire !== null) onChange(wire);
              }}
            />
          </div>
        </div>
      )}
    </div>
  );
}

/** hex 输入框：失焦/回车提交，非法值由 hexToRgbStr 判 null 丢弃 */
function HexInput({ hex, onCommit }: { hex: string; onCommit: (hex: string) => void }) {
  const [text, setText] = useState<string | null>(null);
  return (
    <input
      type="text"
      className="w-20 rounded border border-[var(--separator)] bg-[var(--content)] px-1.5 py-0.5 text-[11.5px] uppercase focus:outline-none focus:ring-1 focus:ring-[var(--accent-strong)]"
      value={text ?? hex}
      onChange={(e) => setText(e.target.value)}
      onBlur={(e) => {
        onCommit(e.target.value);
        setText(null);
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        if (e.key === "Escape") setText(null);
      }}
    />
  );
}

// ---------- slider：通栏轨道 + 标签行右端的可编辑数值（精度随 step） ----------

/** 滑条参数：精度优先级 project.json step > precision 推导 > 缺省 0.01；
 *  显示小数位优先取 precision（真实数据 51 处声明，如 min=-100..100 precision=1） */
function sliderSpec(p: WebPropDef) {
  const min = p.min ?? 0;
  const max = p.max ?? 1;
  const precisionStep = p.precision !== undefined ? Math.pow(10, -p.precision) : undefined;
  const step = p.step ?? precisionStep ?? 0.01;
  const decimals = p.precision ?? (String(step).split(".")[1] ?? "").length;
  const clamp = (n: number) => Math.min(max, Math.max(min, n));
  return { min, max, step, decimals, clamp };
}

function SliderTrack({
  p,
  value,
  onChange,
}: {
  p: WebPropDef;
  value: WebPropValues[string] | undefined;
  onChange: (v: number) => void;
}) {
  const { min, max, step, clamp } = sliderSpec(p);
  return (
    <input
      type="range"
      className="w-full accent-[var(--accent-strong)]"
      min={min}
      max={max}
      step={step}
      value={clamp(Number(value ?? 0))}
      onChange={(e) => onChange(Number(e.target.value))}
    />
  );
}

function SliderValueInput({
  p,
  value,
  onChange,
}: {
  p: WebPropDef;
  value: WebPropValues[string] | undefined;
  onChange: (v: number) => void;
}) {
  const { decimals, clamp } = sliderSpec(p);
  const cur = clamp(Number(value ?? 0));
  // 编辑中的原始文本（null = 未在打字，直接显示格式化值）
  const [text, setText] = useState<string | null>(null);

  const commit = (raw: string) => {
    const n = Number(raw);
    if (raw.trim() !== "" && Number.isFinite(n)) onChange(clamp(n));
    setText(null);
  };

  return (
    <input
      type="text"
      className="w-14 shrink-0 rounded-md border border-[var(--separator)] bg-[var(--card)] px-1.5 py-0.5 text-right text-[11.5px] tabular-nums focus:outline-none focus:ring-1 focus:ring-[var(--accent-strong)]"
      value={text ?? cur.toFixed(decimals)}
      onChange={(e) => setText(e.target.value)}
      onBlur={(e) => commit(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        if (e.key === "Escape") setText(null);
      }}
    />
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
          {tr("{v}（当前值不在可选范围）", { v: String(cur) })}
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
//
// 控件与主窗口「设置 → 画质」同一套设计（档位按钮 + 滑条），只是每一行
// 多一层「跟随全局 / 专属」语义：滑条显示生效值（覆盖 ?? 全局），拖动即写
// 本壁纸专属值，点「专属 ×」回到跟随全局。

const FIT_LABELS: Record<string, string> = {
  // 与设置页、托盘菜单同一套说法（cover/contain/stretch → 裁剪/缩放/拉伸），
  // 同一个值在三处叫法必须一致，否则用户以为是不同的东西
  cover: "裁剪",
  contain: "缩放",
  stretch: "拉伸",
};
/** 抗锯齿已锁定为关（方案优化中）：不提供档位选择，见下方锁定行。
 *  优化完成后恢复 off/fxaa/msaa2/msaa4 四档（与设置页、Rust 的 AA_CHOICES 一致） */

/** 滑条行统一取值：覆盖值优先，缺失跟随全局（清晰度历史 0=自动按原生显示） */
function effDpr(play: ItemPlayConfig, globals: PlayConfigGlobals): number {
  const v = play.renderDpr;
  return v !== undefined && v > 0 ? v : globals.renderDpr || 1;
}
function effResources(play: ItemPlayConfig, globals: PlayConfigGlobals): number {
  return play.resources ?? globals.resources ?? 1;
}

function PlayConfigPanel({
  play,
  globals,
  errMsg,
  onChange,
  onResetAll,
  onApplyPreset,
  onFollowQuality,
  overrideCount,
}: {
  play: ItemPlayConfig;
  globals: PlayConfigGlobals | null;
  errMsg: string;
  onChange: (patch: ItemPlayConfig) => void;
  onResetAll: () => void;
  /** 画质档位一键套用：把六个画质参数写成本壁纸专属值（抗锯齿不动） */
  onApplyPreset: (id: "low" | "medium" | "high") => void;
  /** 画质档位行「专属 ×」：六个画质参数全部回到跟随全局 */
  onFollowQuality: () => void;
  overrideCount: number;
}) {
  if (!globals) {
    return (
      <div className="flex flex-1 items-center justify-center text-[13px] text-[var(--text-2)]">
        {errMsg ? (
          <span className="text-red-500">
            {tr("读取失败")}: {trMsg(errMsg)}
          </span>
        ) : (
          tr("正在读取…")
        )}
      </div>
    );
  }
  // 档位反推按**生效值**（覆盖 ?? 全局）：与设置页同规则，任一参数不是预设值
  // 即「自定义」。六个画质参数任一为专属值时档位行亮「专属」标记
  const preset = deriveQualityPreset({
    renderDpr: effDpr(play, globals),
    sceneFps: play.sceneFps ?? globals.sceneFps,
    particles: play.particles ?? globals.particles,
    post: play.postProcessing ?? globals.postProcessing,
    resources: effResources(play, globals),
    resourcesNormal: play.resourcesNormal ?? globals.resourcesNormal,
  });
  const qualityOverridden = (
    ["renderDpr", "sceneFps", "particles", "postProcessing", "resources", "resourcesNormal"] as const
  ).some((k) => play[k] !== undefined);
  return (
    <>
      <div className="flex-1 overflow-y-auto px-5 py-3">
        <div className="mb-3 text-[11.5px] leading-relaxed text-[var(--text-2)]">
          {tr(
            "这些设置只作用于本张壁纸，切换壁纸后各自保留。选「跟随全局」则使用设置 → 画质里的值。",
          )}
        </div>
        <div className="flex flex-col gap-3">
          <PlayRow
            label={tr("画质档位")}
            desc={
              preset === "low"
                ? tr("低：省电优先 — 清晰度 0.75 · 15 FPS · 粒子/后处理低 · 贴图 60% · 法线 75%")
                : preset === "medium"
                  ? tr("中：均衡 — 清晰度 0.85 · 30 FPS · 粒子/后处理中 · 贴图 80%")
                  : preset === "high"
                    ? tr("高：画质优先 — 清晰度 1.0 · 30 FPS · 粒子/后处理高 · 贴图/法线原生")
                    : tr("自定义：手动调整下方任意参数即进入自定义。点档位一键套用预设，整体覆盖下方画质参数（显示模式/音量不动）")
            }
            isOverride={qualityOverridden}
            onFollow={onFollowQuality}
          >
            <div className="flex overflow-hidden rounded-lg border border-[var(--separator)]">
              {(["low", "medium", "high", "custom"] as const).map((id) => (
                <button
                  key={id}
                  type="button"
                  disabled={id === "custom" && preset !== "custom"}
                  onClick={() => id !== "custom" && onApplyPreset(id)}
                  className={`px-3 py-1 text-[12.5px] transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${
                    preset === id
                      ? "bg-[var(--accent-fill)] text-white"
                      : "bg-[var(--card)] text-[var(--text-2)] hover:text-[var(--text)]"
                  }`}
                >
                  {tr(PRESET_LABELS[id])}
                </button>
              ))}
            </div>
          </PlayRow>

          <PlayRow
            label={tr("显示模式")}
            desc={tr("画面与屏幕比例不一致时如何填充")}
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
              <option value="">
                {tr("跟随全局（{v}）", { v: tr(FIT_LABELS[globals.fit] ?? globals.fit) })}
              </option>
              {Object.entries(FIT_LABELS).map(([v, label]) => (
                <option key={v} value={v}>
                  {tr(label)}
                </option>
              ))}
            </select>
          </PlayRow>

          <PlayRow
            label={tr("清晰度")}
            desc={tr("渲染分辨率相对屏幕像素比的倍率（0.50–1.00，1=原生），越低越省显存")}
            isOverride={play.renderDpr !== undefined}
            onFollow={() => onChange({ renderDpr: undefined })}
          >
            <div className="flex items-center gap-2">
              <input
                type="range"
                min={0.5}
                max={1}
                step={0.05}
                value={effDpr(play, globals)}
                onChange={(e) => onChange({ renderDpr: Number(e.target.value) })}
                className={playSliderCls}
              />
              <span className="w-12 text-right text-[12px] tabular-nums text-[var(--text-2)]">
                {effDpr(play, globals) >= 1 ? tr("原生") : `×${effDpr(play, globals).toFixed(2)}`}
              </span>
            </div>
          </PlayRow>

          <PlayRow
            label={tr("贴图倍率")}
            desc={tr("贴图解码/上传的分辨率倍率（0.50–1.00，1=原生）：省显存，画面逐像素不变。改动后本壁纸重载一次")}
            isOverride={play.resources !== undefined}
            onFollow={() => onChange({ resources: undefined })}
          >
            <div className="flex items-center gap-2">
              <input
                type="range"
                min={0.5}
                max={1}
                step={0.05}
                value={effResources(play, globals)}
                onChange={(e) => onChange({ resources: Number(e.target.value) })}
                className={playSliderCls}
              />
              <span className="w-12 text-right text-[12px] tabular-nums text-[var(--text-2)]">
                {effResources(play, globals) >= 1
                  ? tr("原生")
                  : `×${effResources(play, globals).toFixed(2)}`}
              </span>
            </div>
          </PlayRow>

          <PlayRow
            label={tr("法线倍率")}
            desc={tr("法线/蒙版贴图的分辨率倍率（0.35–1.00，默认 1 不缩）：折射与光照对模糊敏感，非必要不动。改动后本壁纸重载一次")}
            isOverride={play.resourcesNormal !== undefined}
            onFollow={() => onChange({ resourcesNormal: undefined })}
          >
            <div className="flex items-center gap-2">
              <input
                type="range"
                min={0.35}
                max={1}
                step={0.05}
                value={play.resourcesNormal ?? globals.resourcesNormal}
                onChange={(e) => onChange({ resourcesNormal: Number(e.target.value) })}
                className={playSliderCls}
              />
              <span className="w-12 text-right text-[12px] tabular-nums text-[var(--text-2)]">
                {(play.resourcesNormal ?? globals.resourcesNormal) >= 1
                  ? tr("原生")
                  : `×${(play.resourcesNormal ?? globals.resourcesNormal).toFixed(2)}`}
              </span>
            </div>
          </PlayRow>

          <PlayRow
            label={tr("帧率上限")}
            desc={tr("场景动画的帧率上限（15–60 FPS 任意值）：越低 GPU 占用越低")}
            isOverride={play.sceneFps !== undefined}
            onFollow={() => onChange({ sceneFps: undefined })}
          >
            <div className="flex items-center gap-2">
              <input
                type="range"
                min={15}
                max={60}
                step={1}
                value={play.sceneFps ?? globals.sceneFps}
                onChange={(e) => onChange({ sceneFps: Number(e.target.value) })}
                className={playSliderCls}
              />
              <span className="w-14 text-right text-[12px] tabular-nums text-[var(--text-2)]">
                {play.sceneFps ?? globals.sceneFps} FPS
              </span>
            </div>
          </PlayRow>

          <PlayRow
            label={tr("抗锯齿")}
            desc={tr("抗锯齿方案优化中：当前所有档位一律关闭且禁止更改，后续版本开放")}
            isOverride={play.aa !== undefined}
            onFollow={() => onChange({ aa: undefined })}
          >
            <select disabled value="off" className={`${playSelectCls} opacity-60`}>
              <option value="off">{tr("关（已锁定）")}</option>
            </select>
          </PlayRow>

          <PlayRow
            label={tr("粒子")}
            desc={tr("雨/雪/火花/雾等粒子数量；低/中档按比例缩数量与发射率，关=不渲染")}
            isOverride={play.particles !== undefined}
            onFollow={() => onChange({ particles: undefined })}
          >
            <div className="flex items-center gap-2">
              <input
                type="range"
                min={0}
                max={3}
                step={1}
                value={Math.max(
                  0,
                  PARTICLE_STOPS.indexOf(
                    (play.particles ?? globals.particles) as (typeof PARTICLE_STOPS)[number],
                  ),
                )}
                onChange={(e) =>
                  onChange({
                    particles: PARTICLE_STOPS[Number(e.target.value)] as ItemPlayConfig["particles"],
                  })
                }
                className={playSliderCls}
              />
              <span className="w-8 text-right text-[12px] text-[var(--text-2)]">
                {tr(stopLabel(play.particles ?? globals.particles))}
              </span>
            </div>
          </PlayRow>

          <PlayRow
            label={tr("后处理")}
            desc={tr("辉光/模糊/水波等画面效果；低/中档压效果链分辨率，关=效果链直通")}
            isOverride={play.postProcessing !== undefined}
            onFollow={() => onChange({ postProcessing: undefined })}
          >
            <div className="flex items-center gap-2">
              <input
                type="range"
                min={0}
                max={3}
                step={1}
                value={Math.max(
                  0,
                  POST_STOPS.indexOf(
                    (play.postProcessing ?? globals.postProcessing) as (typeof POST_STOPS)[number],
                  ),
                )}
                onChange={(e) =>
                  onChange({
                    postProcessing: POST_STOPS[
                      Number(e.target.value)
                    ] as ItemPlayConfig["postProcessing"],
                  })
                }
                className={playSliderCls}
              />
              <span className="w-8 text-right text-[12px] text-[var(--text-2)]">
                {tr(stopLabel(play.postProcessing ?? globals.postProcessing))}
              </span>
            </div>
          </PlayRow>

          <PlayRow
            label={tr("音量")}
            desc={tr("壁纸自带音频的音量；0 即静音。默认静音，避免多张壁纸同时出声")}
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
                className="w-28 accent-[var(--accent-strong)]"
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
            ? `${tr("保存失败")}: ${trMsg(errMsg)}`
            : overrideCount > 0
              ? tr("{n} 项为本壁纸专属", { n: overrideCount })
              : tr("全部跟随全局设置")}
        </span>
        <button
          className="btn ml-auto !py-1 !text-[12px]"
          disabled={overrideCount === 0}
          onClick={onResetAll}
        >
          ↺ {tr("全部跟随全局")}
        </button>
      </div>
    </>
  );
}

const playSelectCls =
  "rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1.5 text-[12.5px] focus:outline-none focus:ring-1 focus:ring-[var(--accent-strong)]";
// 播放设置滑条：与设置页画质滑条同观感（窄一点，行内右对齐）
const playSliderCls = "w-32 accent-[var(--accent-strong)]";

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
              className="rounded border border-[var(--separator)] bg-[var(--accent)] px-1.5 text-[10px] text-[var(--accent-strong)] hover:opacity-80"
              onClick={onFollow}
              title={tr("点击恢复为跟随全局")}
            >
              {tr("专属")} ×
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
