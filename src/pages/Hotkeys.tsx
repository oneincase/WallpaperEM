// 快捷键页：7 个动作的全局/菜单热键管理（默认值 + 自定义录制 + 恢复默认）。
//
// 录制的实现要点（macOS 的坑，需求里点过名）：
//   ⌘+任意组合在 webview 里通常**到不了 keydown** —— 被应用菜单键位（⌘M/⌘H/
//   ⌘C…）或已注册的全局热键先吞掉。所以录制期间调 hotkeys_record_begin
//   （摘菜单 + 注销全部全局热键），录完 record_end 反向恢复；Esc = 取消录制。
//   这样 ⌘+单键、⌘+多修饰键都能录到。
//
// 冲突处理（「被占用了需要有提示，可以取消或者覆盖」）：
//   - 与其它动作的绑定冲突 → 弹确认框：取消 / 覆盖（从对方移走并绑到本项）；
//   - 与系统/菜单保留组合（⌘Q 等）冲突 → 弹确认框：取消 / 覆盖（force 落盘）。
import { useCallback, useEffect, useRef, useState } from "react";
import { api, type HotkeyItem, type HotkeysInfo } from "../api/steam";
import { tr, trMsg, useLocale } from "../lib/i18n";
import { ConfirmModal } from "../components/ConfirmModal";
import { useMessage } from "../components/Message";

/** KeyboardEvent → 绑定串（如 "cmd+shift+p"）。返回 null = 纯修饰键/不认识，继续等 */
function comboFromEvent(e: KeyboardEvent): string | null {
  // 只按物理键位（e.code）翻译：与 Rust 侧 parse_key 的键名表一一对应
  // （KeyA / Digit1 / F5 / ArrowUp / Space…原样小写化即可识别）
  const code = e.code;
  if (
    code === "MetaLeft" || code === "MetaRight" || code === "ControlLeft" ||
    code === "ControlRight" || code === "AltLeft" || code === "AltRight" ||
    code === "ShiftLeft" || code === "ShiftRight" || code === "CapsLock" ||
    code === ""
  ) {
    return null; // 纯修饰键，等主键
  }
  const mods: string[] = [];
  if (e.metaKey) mods.push("cmd");
  if (e.ctrlKey) mods.push("ctrl");
  if (e.altKey) mods.push("alt");
  if (e.shiftKey) mods.push("shift");
  // KeyA → a、Digit1 → 1、F5 → f5、ArrowUp → arrowup、Space → space
  let key = code.replace(/^Key/, "").replace(/^Digit/, "").toLowerCase();
  if (key === "") return null;
  return [...mods, key].join("+");
}

/** 绑定串 → 展示文案（mac 用符号，其余用 Ctrl+Shift+P 式） */
function prettyCombo(accel: string): string {
  const isMac = navigator.platform.toLowerCase().includes("mac");
  return accel
    .split("+")
    .map((p) => {
      switch (p) {
        case "cmd":
          return isMac ? "⌘" : "Win";
        case "ctrl":
          return isMac ? "⌃" : "Ctrl";
        case "alt":
          return isMac ? "⌥" : "Alt";
        case "shift":
          return isMac ? "⇧" : "Shift";
        default:
          return p.length === 1 ? p.toUpperCase() : p.replace(/^f(\d+)$/i, "F$1");
      }
    })
    .join(isMac ? "" : "+");
}

interface PendingConflict {
  accel: string;
  /** 冲突方动作名（空 = 系统/菜单保留组合） */
  occupier: string;
  reserved: boolean;
}

export function HotkeysPage() {
  useLocale();
  const msg = useMessage();
  const [info, setInfo] = useState<HotkeysInfo | null>(null);
  const [recording, setRecording] = useState<string | null>(null); // 动作 id
  const [conflict, setConflict] = useState<PendingConflict | null>(null);
  const recordingRef = useRef<string | null>(null);
  recordingRef.current = recording;

  const reload = useCallback(async () => {
    try {
      setInfo(await api.hotkeysList());
    } catch (e) {
      msg.error(tr("快捷键读取失败：{err}", { err: trMsg(String(e)) }));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // ---- 录制 ----
  const finishRecording = useCallback(async () => {
    setRecording(null);
    try {
      await api.hotkeysRecordEnd();
    } catch {
      // 恢复失败只影响热键可用性（下次启动自愈），不打断用户
    }
    await reload();
  }, [reload]);

  const commitCombo = useCallback(
    async (action: string, accel: string, force: boolean) => {
      await api.hotkeysSet(action, [accel], force);
      msg.success(tr("快捷键已更新"));
      await finishRecording();
    },
    [finishRecording, msg],
  );

  const onCaptured = useCallback(
    (action: string, accel: string, info2: HotkeysInfo) => {
      // 冲突检测：其它动作的绑定 / 系统保留组合
      const occupier = info2.items.find(
        (it) => it.id !== action && it.bindings.includes(accel),
      );
      const reserved = info2.reserved.includes(accel);
      if (occupier || reserved) {
        setConflict({
          accel,
          occupier: occupier?.label ?? "",
          reserved: !!reserved && !occupier,
        });
        return; // 弹框里再决定取消/覆盖，录制态先挂着
      }
      void commitCombo(action, accel, false).catch((e) => {
        msg.error(tr("快捷键设置失败：{err}", { err: trMsg(String(e)) }));
        void finishRecording();
      });
    },
    [commitCombo, finishRecording, msg],
  );

  const startRecording = useCallback(
    async (action: string) => {
      setRecording(action);
      try {
        await api.hotkeysRecordBegin();
      } catch (e) {
        setRecording(null);
        msg.error(tr("录制准备失败：{err}", { err: trMsg(String(e)) }));
      }
    },
    [msg],
  );

  // 录制态下的按键采集：capture 阶段拦截，避免页面其它快捷键逻辑抢跑
  useEffect(() => {
    if (!recording) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.code === "Escape") {
        void finishRecording();
        return;
      }
      const accel = comboFromEvent(e);
      if (!accel || !info) return;
      // 至少一个修饰键（或 F 键）才合法 —— 与 Rust validate_accel 同规则
      const hasMod = e.metaKey || e.ctrlKey || e.altKey || e.shiftKey;
      if (!hasMod && !/^f\d+$/i.test(accel)) {
        msg.info(tr("请配合修饰键（⌘/Ctrl/Alt/Shift）或使用 F 键"));
        return;
      }
      onCaptured(recording, accel, info);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [recording, info, finishRecording, onCaptured, msg]);

  // 卸载兜底：录制中途切页/关窗时也要恢复菜单与热键（record_end 幂等，多调无害）
  useEffect(() => {
    return () => {
      if (recordingRef.current) {
        recordingRef.current = null;
        void api.hotkeysRecordEnd().catch(() => {});
      }
    };
  }, []);

  // ---- 冲突弹框 ----
  const resolveConflict = useCallback(
    async (override: boolean) => {
      const c = conflict;
      const action = recording;
      setConflict(null);
      if (!c || !action) return;
      if (!override) {
        await finishRecording(); // 取消：保留原绑定
        return;
      }
      if (c.reserved) {
        // 系统/菜单保留组合：force 落盘（能不能真注册上听天由命）
        void commitCombo(action, c.accel, true).catch((e) => {
          msg.error(tr("快捷键设置失败：{err}", { err: trMsg(String(e)) }));
          void finishRecording();
        });
        return;
      }
      // 覆盖：从冲突方移走，绑到本项
      if (info) {
        const other = info.items.find((it) => it.label === c.occupier);
        if (other) {
          try {
            await api.hotkeysSet(
              other.id,
              other.bindings.filter((b) => b !== c.accel),
            );
          } catch (e) {
            msg.error(tr("快捷键设置失败：{err}", { err: trMsg(String(e)) }));
            await finishRecording();
            return;
          }
        }
      }
      void commitCombo(action, c.accel, false).catch((e) => {
        msg.error(tr("快捷键设置失败：{err}", { err: trMsg(String(e)) }));
        void finishRecording();
      });
    },
    [conflict, recording, info, commitCombo, finishRecording, msg],
  );

  const resetOne = useCallback(
    async (action: string) => {
      try {
        await api.hotkeysReset(action);
        msg.success(tr("已恢复默认快捷键"));
        await reload();
      } catch (e) {
        msg.error(tr("恢复默认失败：{err}", { err: trMsg(String(e)) }));
      }
    },
    [reload, msg],
  );

  return (
    <div className="mx-auto flex h-full w-full max-w-[760px] flex-col px-6 py-5">
      <div className="mb-1 text-[17px] font-semibold">{tr("快捷键")}</div>
      <div className="mb-4 text-[12px] leading-relaxed text-[var(--text-2)]">
        {tr(
          "全局生效：游戏/其它应用在前台也能用。⌘M、⌘H 这类 macOS 系统惯例键只在 WallpaperEM 内生效（避免劫持其它应用）；主窗口隐藏后用 ⌘⇧M 全局唤回。点「录制」后按下新组合键，Esc 取消。",
        )}
      </div>

      {!info ? (
        <div className="flex flex-1 items-center justify-center text-[13px] text-[var(--text-2)]">
          {tr("正在读取…")}
        </div>
      ) : (
        <div className="flex flex-col">
          {info.items.map((it) => (
            <HotkeyRow
              key={it.id}
              item={it}
              recording={recording === it.id}
              onRecord={() => void startRecording(it.id)}
              onCancelRecord={() => void finishRecording()}
              onReset={() => void resetOne(it.id)}
            />
          ))}
        </div>
      )}

      {conflict && (
        <ConfirmModal
          title={tr("快捷键冲突")}
          message={
            conflict.reserved
              ? tr(
                  "「{combo}」是系统/菜单保留组合，占用后可能不生效或引发异常。仍然覆盖？",
                  { combo: prettyCombo(conflict.accel) },
                )
              : tr("「{combo}」已被「{who}」占用。覆盖后将从对方移除。", {
                  combo: prettyCombo(conflict.accel),
                  who: conflict.occupier,
                })
          }
          confirmText={tr("覆盖")}
          cancelText={tr("取消")}
          onConfirm={() => void resolveConflict(true)}
          onCancel={() => void resolveConflict(false)}
        />
      )}
    </div>
  );
}

function HotkeyRow({
  item,
  recording,
  onRecord,
  onCancelRecord,
  onReset,
}: {
  item: HotkeyItem;
  recording: boolean;
  onRecord: () => void;
  onCancelRecord: () => void;
  onReset: () => void;
}) {
  return (
    <div className="flex items-center justify-between gap-4 border-b border-[var(--separator)] py-3 last:border-0">
      <div className="min-w-0">
        <div className="text-[14px] font-medium">{tr(item.label)}</div>
        <div className="mt-1 flex flex-wrap items-center gap-1.5">
          {recording ? (
            <span className="rounded-lg border border-[var(--accent-strong)] bg-[var(--accent)] px-2.5 py-1 text-[12px] text-[var(--accent-strong)]">
              {tr("按下快捷键…（Esc 取消）")}
            </span>
          ) : item.bindings.length === 0 ? (
            <span className="text-[12px] text-[var(--text-2)]">{tr("未绑定")}</span>
          ) : (
            item.bindings.map((b) => (
              <span
                key={b}
                className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2.5 py-1 text-[12px] tabular-nums"
              >
                {prettyCombo(b)}
              </span>
            ))
          )}
        </div>
      </div>
      <div className="flex shrink-0 items-center gap-1.5">
        {recording ? (
          <button className="btn !py-1 text-[12px]" onClick={onCancelRecord}>
            {tr("取消")}
          </button>
        ) : (
          <>
            <button className="btn !py-1 text-[12px]" onClick={onRecord}>
              {tr("录制")}
            </button>
            <button
              className="btn !py-1 text-[12px]"
              onClick={onReset}
              title={tr("恢复默认")}
            >
              ↺ {tr("默认")}
            </button>
          </>
        )}
      </div>
    </div>
  );
}
