// 显示器管理页：布局舞台（微缩屏用真实壁纸封面填充）+ 每屏设备卡 + 统一/独立模式。
//
// 统一模式（默认）：应用壁纸 = 刷全部屏；轮播一条全局运行条控制。
// 独立模式：每屏各自指定壁纸 / 绑定各自的切换列表（锚定菜单，不弹框），
// 卡片上直接显示进度与单屏切歌。每屏「更换壁纸」在两种模式下都是显式单屏操作：
// 锁定目标屏 → 跳本地库挑选。
import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  type DisplayInfo,
  type DisplaysListResult,
  type Playlist,
  type PlaylistStatus,
} from "../api/steam";
import { useMessage } from "../components/Message";
import { AnchoredMenu, MenuItem } from "../components/AnchoredMenu";
import { IconMonitor } from "../components/icons";
import { armApplyTarget } from "../lib/apply-target";
import { formatCountdown } from "../lib/format";
import { tr } from "../lib/i18n";

/** 轮播倒计时进度（0..1）：距下次切换的比例，用于细进度条 */
function rotProgress(nextAtMs: number | null | undefined, intervalSec: number, nowMs: number) {
  if (!nextAtMs || intervalSec <= 0) return 0;
  const left = nextAtMs - nowMs;
  return Math.min(1, Math.max(0, 1 - left / (intervalSec * 1000)));
}

export function DisplaysPage({ onNavigate }: { onNavigate: (p: "library") => void }) {
  const msg = useMessage();
  const [data, setData] = useState<DisplaysListResult | null>(null);
  const [rot, setRot] = useState<PlaylistStatus | null>(null);
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [now, setNow] = useState(Date.now());
  // 每屏「轮播列表」锚定菜单（不弹框）
  const [bindAt, setBindAt] = useState<{ x: number; y: number; d: DisplayInfo } | null>(null);

  const load = useCallback(async () => {
    try {
      const [d, r, ls] = await Promise.all([
        api.wallpaperDisplaysList(),
        api.playlistStatus(),
        api.playlistList(),
      ]);
      setData(d);
      setRot(r);
      setPlaylists(ls);
    } catch (e) {
      msg.error(String(e));
    }
  }, [msg]);

  useEffect(() => {
    void load();
  }, [load]);

  // 倒计时每秒本地走；状态 20s 对齐一次（自动切换后 index/nextAt 会变）
  useEffect(() => {
    const t1 = window.setInterval(() => setNow(Date.now()), 1000);
    const t2 = window.setInterval(() => void load(), 20_000);
    return () => {
      window.clearInterval(t1);
      window.clearInterval(t2);
    };
  }, [load]);

  // 后端在应用/停止/热插拔时推送事件，页面随之刷新
  useEffect(() => {
    const unsubs: Array<() => void> = [];
    void listen("displays-changed", () => void load()).then((un) => unsubs.push(un));
    void listen("sessions-changed", () => void load()).then((un) => unsubs.push(un));
    // 托盘「轮播」子菜单改了暂停状态：统一模式的暂停/恢复钮立即跟上
    void listen<{ key: string }>("settings-changed", (e) => {
      if (e.payload.key === "playlist_rotation_paused") void load();
    }).then((un) => unsubs.push(un));
    return () => {
      for (const un of unsubs) un();
    };
  }, [load]);

  const setMode = useCallback(
    async (m: "unified" | "independent") => {
      try {
        await invoke("settings_set", { key: "display_mode", value: m });
        setData((d) => (d ? { ...d, mode: m } : d));
        msg.success(m === "unified" ? tr("已切换到统一模式") : tr("已切换到独立模式"));
      } catch (e) {
        msg.error(String(e));
      }
    },
    [msg],
  );

  const stopAll = useCallback(async () => {
    try {
      await api.wallpaperStop();
      msg.success(tr("已停止全部壁纸"));
      await load();
    } catch (e) {
      msg.error(String(e));
    }
  }, [msg, load]);

  const rotAct = useCallback(
    async (fn: () => Promise<unknown>) => {
      try {
        await fn();
        await load();
      } catch (e) {
        msg.error(String(e));
      }
    },
    [msg, load],
  );

  const bind = useCallback(
    async (d: DisplayInfo, playlistId: number | null) => {
      setBindAt(null);
      try {
        await api.displayBindingSet(d.id, playlistId);
        msg.success(
          playlistId === null
            ? tr("「{name}」已固定为当前壁纸", { name: d.name })
            : tr("「{name}」开始轮播", { name: d.name }),
        );
        await load();
      } catch (e) {
        msg.error(String(e));
      }
    },
    [msg, load],
  );

  const mode = data?.mode ?? "unified";
  const displays = data?.displays ?? [];
  const unifiedRot = rot?.active && mode !== "independent" ? rot : null;

  return (
    <div className="h-full overflow-y-auto px-7 py-5">
      {/* 页头 */}
      <header className="mb-5 flex items-end justify-between gap-4">
        <div>
          <h1 className="text-[22px] font-bold tracking-tight">{tr("显示器")}</h1>
          <p className="mt-0.5 text-[13px] text-[var(--text-2)]">
            {tr("每块屏独立管理壁纸；「更换壁纸」只作用于所选屏")}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <ModeToggle mode={mode} onChange={(m) => void setMode(m)} />
          <button
            className="btn btn-danger !py-1 text-[12px]"
            title={tr("停止所有屏的壁纸")}
            onClick={() => void stopAll()}
          >
            {tr("全部停止")}
          </button>
        </div>
      </header>
      <p className="mb-4 text-[12px] text-[var(--text-2)]">
        {mode === "independent"
          ? tr("独立模式：每块屏可各自指定壁纸（与后续各自的切换列表）；点「应用」时选择目标屏")
          : tr("统一模式：应用壁纸时同步替换所有显示器的壁纸")}
      </p>

      {/* 布局舞台：微缩屏 = 真实壁纸封面 */}
      <section
        className="card relative overflow-hidden p-5"
        style={{
          background:
            "radial-gradient(130% 160% at 22% -20%, var(--card) 0%, var(--content) 70%)",
        }}
      >
        <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
          <span className="text-[13px] font-semibold text-[var(--text-2)]">
            {tr("显示器布局")}
          </span>
          {unifiedRot && (
            <span className="flex items-center gap-2 rounded-full bg-[var(--accent)]/10 px-3 py-1 text-[12px] text-[var(--text-2)]">
              <span className="text-[var(--accent-strong)]">▶</span>
              <span className="font-medium text-[var(--text-1)]">
                {tr("轮播：{name}（{i}/{t}）", {
                  name: unifiedRot.name ?? "",
                  i: (unifiedRot.index ?? 0) + 1,
                  t: unifiedRot.total ?? 0,
                })}
              </span>
              <span>
                {unifiedRot.paused
                  ? tr("已暂停自动切换")
                  : unifiedRot.nextAtMs
                    ? tr("{cd} 后切换", { cd: formatCountdown(unifiedRot.nextAtMs - now) })
                    : ""}
              </span>
              <span className="mx-0.5 h-3 w-px bg-[var(--separator)]" />
              <button
                title={tr("上一张")}
                className="hover:text-[var(--accent-strong)]"
                onClick={() => void rotAct(() => api.wallpaperPrev())}
              >
                ‹
              </button>
              <button
                title={tr("下一张")}
                className="hover:text-[var(--accent-strong)]"
                onClick={() => void rotAct(() => api.wallpaperNext())}
              >
                ›
              </button>
              <button
                title={unifiedRot.paused ? tr("恢复轮播") : tr("暂停轮播")}
                className="hover:text-[var(--accent-strong)]"
                onClick={() => void rotAct(() => api.wallpaperRotationSet(!unifiedRot.paused))}
              >
                {unifiedRot.paused ? "▶" : "⏸"}
              </button>
            </span>
          )}
        </div>
        <LayoutStage displays={displays} nowMs={now} />
      </section>

      {/* 每屏设备卡 */}
      <div className="mt-4 space-y-3">
        {displays.map((d) => (
          <DisplayCard
            key={d.id}
            d={d}
            nowMs={now}
            onNavigate={onNavigate}
            onChanged={() => void load()}
            onBind={(x, y) => setBindAt({ x, y, d })}
          />
        ))}
      </div>

      {bindAt && (
        <AnchoredMenu x={bindAt.x} y={bindAt.y} align="right" onClose={() => setBindAt(null)}>
          <MenuItem
            selected={bindAt.d.binding == null}
            onClick={() => void bind(bindAt.d, null)}
          >
            {tr("固定（不轮播）")}
          </MenuItem>
          {playlists.map((p) => (
            <MenuItem
              key={p.id}
              selected={bindAt.d.binding?.playlistId === p.id}
              onClick={() => void bind(bindAt.d, p.id)}
            >
              {p.name}
              <span className="ml-1 text-[10.5px] text-[var(--text-2)]">
                {tr("{n} 项", { n: p.itemIds.length })}
              </span>
            </MenuItem>
          ))}
          {playlists.length === 0 && (
            <div className="px-3 py-1.5 text-[12px] text-[var(--text-2)]">
              {tr("还没有切换列表，先到本地库新建")}
            </div>
          )}
        </AnchoredMenu>
      )}
    </div>
  );
}

/** 模式分段控件：滑块高亮 + 平移过渡 */
function ModeToggle({
  mode,
  onChange,
}: {
  mode: string;
  onChange: (m: "unified" | "independent") => void;
}) {
  const independent = mode === "independent";
  return (
    <div className="relative flex rounded-full border border-[var(--separator)] bg-[var(--card)] p-0.5 text-[12px] shadow-sm">
      <span
        className="absolute bottom-0.5 top-0.5 rounded-full bg-[var(--accent-fill)] transition-all duration-200 ease-out"
        style={{ width: "calc(50% - 2px)", left: independent ? "calc(50% + 1px)" : "2px" }}
      />
      <button
        onClick={() => onChange("unified")}
        className={`relative z-10 rounded-full px-3 py-1 transition-colors ${
          independent ? "text-[var(--text-2)]" : "text-[var(--content)]"
        }`}
      >
        {tr("统一模式")}
      </button>
      <button
        onClick={() => onChange("independent")}
        className={`relative z-10 rounded-full px-3 py-1 transition-colors ${
          independent ? "text-[var(--content)]" : "text-[var(--text-2)]"
        }`}
      >
        {tr("独立模式")}
      </button>
    </div>
  );
}

/** 布局舞台：按真实坐标排布微缩屏，封面即壁纸缩略图 */
function LayoutStage({ displays, nowMs }: { displays: DisplayInfo[]; nowMs: number }) {
  if (displays.length === 0) {
    return (
      <div className="py-14 text-center text-[13px] text-[var(--text-2)]">
        {tr("未检测到显示器")}
      </div>
    );
  }
  const minX = Math.min(...displays.map((d) => d.x));
  const minY = Math.min(...displays.map((d) => d.y));
  const maxX = Math.max(...displays.map((d) => d.x + d.w));
  const maxY = Math.max(...displays.map((d) => d.y + d.h));
  const bw = Math.max(maxX - minX, 1);
  const bh = Math.max(maxY - minY, 1);

  return (
    <div className="mx-auto w-full" style={{ aspectRatio: `${bw} / ${bh}`, maxHeight: 240 }}>
      {displays.map((d) => {
        const b = d.binding;
        const pct = rotProgress(b?.nextAtMs, b?.intervalSec ?? 0, nowMs);
        return (
          <button
            key={d.id}
            title={`${d.name} · ${Math.round(d.w)}×${Math.round(d.h)}`}
            onClick={() =>
              document
                .getElementById(`display-card-${d.id}`)
                ?.scrollIntoView({ behavior: "smooth", block: "center" })
            }
            className="group absolute p-[3px]"
            style={{
              left: `${((d.x - minX) / bw) * 100}%`,
              top: `${((d.y - minY) / bh) * 100}%`,
              width: `${(d.w / bw) * 100}%`,
              height: `${(d.h / bh) * 100}%`,
            }}
          >
            <div
              className={`relative flex h-full w-full flex-col overflow-hidden rounded-xl border-2 bg-[var(--card)] shadow-lg transition-all duration-200 group-hover:-translate-y-0.5 group-hover:shadow-xl ${
                d.isPrimary
                  ? "border-[var(--accent-strong)]"
                  : "border-[var(--separator)] group-hover:border-[var(--accent-strong)]/60"
              }`}
            >
              {/* 屏面 = 当前壁纸封面 */}
              {d.previewUrl ? (
                <img
                  src={d.previewUrl}
                  alt=""
                  className="absolute inset-0 h-full w-full object-cover transition-transform duration-300 group-hover:scale-[1.06]"
                />
              ) : (
                <div className="absolute inset-0 flex items-center justify-center bg-white/5 text-[var(--text-2)] opacity-40">
                  <IconMonitor />
                </div>
              )}
              {/* 底部名字条 */}
              <span className="absolute inset-x-0 bottom-0 truncate bg-gradient-to-t from-black/65 to-transparent px-1.5 pb-0.5 pt-3 text-left text-[9.5px] font-medium text-white/90">
                {d.name}
              </span>
              {/* 主屏徽标 */}
              {d.isPrimary && (
                <span className="absolute right-1 top-1 rounded bg-black/55 px-1 py-px text-[8.5px] font-semibold text-white backdrop-blur-sm">
                  {tr("主屏")}
                </span>
              )}
              {/* 轮播中：脉动点 + 距下次切换的细进度线 */}
              {b && (
                <>
                  <span
                    className="absolute left-1.5 top-1.5 h-2 w-2 animate-pulse rounded-full bg-green-400 shadow"
                    title={tr("轮播：{name} {i}/{t}", {
                      name: b.playlistName,
                      i: b.index + 1,
                      t: b.total,
                    })}
                  />
                  {!b.nextAtMs ? null : (
                    <span className="absolute inset-x-0 bottom-0 h-[3px] bg-black/25">
                      <span
                        className="block h-full bg-[var(--accent-fill)]"
                        style={{ width: `${pct * 100}%` }}
                      />
                    </span>
                  )}
                </>
              )}
            </div>
          </button>
        );
      })}
    </div>
  );
}

/** 每屏设备卡：封面 + 信息 + 轮播胶囊 + 操作簇 */
function DisplayCard({
  d,
  nowMs,
  onNavigate,
  onChanged,
  onBind,
}: {
  d: DisplayInfo;
  nowMs: number;
  onNavigate: (p: "library") => void;
  onChanged: () => void;
  onBind: (x: number, y: number) => void;
}) {
  const msg = useMessage();
  const b = d.binding;
  const pct = rotProgress(b?.nextAtMs, b?.intervalSec ?? 0, nowMs);

  const step = async (forward: boolean) => {
    try {
      if (forward) await api.wallpaperNext(d.id);
      else await api.wallpaperPrev(d.id);
      onChanged();
    } catch (e) {
      msg.error(String(e));
    }
  };
  const act = async (fn: () => Promise<unknown>, ok?: string) => {
    try {
      await fn();
      if (ok) msg.success(ok);
      onChanged();
    } catch (e) {
      msg.error(String(e));
    }
  };

  return (
    <div
      id={`display-card-${d.id}`}
      className="card group flex items-center gap-4 p-3.5 transition-shadow hover:shadow-lg"
    >
      {/* 封面 */}
      <div className="relative h-[76px] w-[124px] shrink-0 overflow-hidden rounded-xl bg-white/5 shadow-inner">
        {d.previewUrl ? (
          <img
            src={d.previewUrl}
            alt=""
            className="h-full w-full object-cover transition-transform duration-300 group-hover:scale-[1.05]"
          />
        ) : (
          <div className="flex h-full w-full items-center justify-center text-[var(--text-2)] opacity-40">
            <IconMonitor />
          </div>
        )}
      </div>

      {/* 信息 */}
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <h3 className="truncate text-[14.5px] font-semibold">{d.name}</h3>
          {d.isPrimary && (
            <span className="rounded bg-[var(--accent)]/15 px-1 py-px text-[10px] text-[var(--accent-strong)]">
              {tr("主屏")}
            </span>
          )}
          <span className="shrink-0 text-[11.5px] text-[var(--text-2)]">
            {Math.round(d.w)}×{Math.round(d.h)} · {d.scale}×
          </span>
        </div>
        <div className="mt-0.5 truncate text-[12.5px] text-[var(--text-2)]">
          {d.title ? tr("当前：{title}", { title: d.title }) : tr("未设置壁纸")}
        </div>
        {b && (
          <div className="mt-1.5 inline-flex items-center gap-2 rounded-full bg-[var(--accent)]/10 px-2.5 py-1 text-[11.5px]">
            <span className="text-[var(--accent-strong)]">▶</span>
            <span className="font-medium text-[var(--text-1)]">
              {tr("轮播：{name} {i}/{t}", { name: b.playlistName, i: b.index + 1, t: b.total })}
            </span>
            <span className="text-[var(--text-2)]">
              {b.nextAtMs
                ? tr("{cd} 后切换", { cd: formatCountdown(b.nextAtMs - nowMs) })
                : tr("已暂停")}
            </span>
            <span className="h-3 w-px bg-[var(--separator)]" />
            <button
              title={tr("上一张")}
              className="text-[13px] leading-3 text-[var(--text-2)] hover:text-[var(--accent-strong)]"
              onClick={() => void step(false)}
            >
              ‹
            </button>
            <button
              title={tr("下一张")}
              className="text-[13px] leading-3 text-[var(--text-2)] hover:text-[var(--accent-strong)]"
              onClick={() => void step(true)}
            >
              ›
            </button>
            {b.nextAtMs ? (
              <span className="h-[3px] w-14 overflow-hidden rounded bg-[var(--separator)]">
                <span
                  className="block h-full bg-[var(--accent-fill)]"
                  style={{ width: `${pct * 100}%` }}
                />
              </span>
            ) : null}
          </div>
        )}
      </div>

      {/* 操作簇 */}
      <div className="flex shrink-0 items-center gap-1.5">
        <button
          className="btn btn-primary !py-1 text-[12px]"
          title={tr("更换壁纸")}
          onClick={() => {
            armApplyTarget({ id: d.id, name: d.name });
            onNavigate("library");
          }}
        >
          {tr("更换壁纸")}
        </button>
        <button
          className="btn !py-1 text-[12px]"
          title={tr("轮播列表")}
          onClick={(e) => {
            const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
            onBind(r.right, r.bottom);
          }}
        >
          {tr("轮播列表")}
        </button>
        {d.itemId && (
          <>
            <button
              className="btn !px-2 !py-1 text-[12px]"
              title={tr("同步到所有屏")}
              onClick={() => void act(() => api.wallpaperApplyItem(d.itemId!), tr("已同步到所有显示器"))}
            >
              ⇔
            </button>
            <button
              className="btn btn-danger !px-2 !py-1 text-[12px]"
              title={tr("停止该屏壁纸")}
              onClick={() => void act(() => api.wallpaperStop(d.id))}
            >
              ⏹
            </button>
          </>
        )}
      </div>
    </div>
  );
}
