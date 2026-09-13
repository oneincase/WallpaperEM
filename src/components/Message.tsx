// 全局轻量消息提示（toast）
//
// 替代散落在各页的 `{msg && <div>…</div>}` 灰字条：那种写法会挤占布局、
// 不会自动消失、也分不出成功/失败。这里做成右上角浮层，所有类型统一 3s
// 自动淡出（也可以点 × 立即关掉）。
//
// 用法：在应用根部挂 <MessageProvider>，页面里 const msg = useMessage()
// 然后 msg.success("已清理 12 个失效条目") / msg.error(String(e))。
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { tr, trMsg } from "../lib/i18n";

type MessageKind = "success" | "error" | "info";

type MessageItem = {
  id: number;
  kind: MessageKind;
  text: string;
};

/** 所有类型的自动消失时长（右上角 toast，3 秒） */
const AUTO_DISMISS_MS = 3000;
/** 同屏最多堆叠数量，超出时挤掉最旧的 */
const MAX_STACK = 3;

type MessageApi = {
  success: (text: string) => void;
  error: (text: string) => void;
  info: (text: string) => void;
  /** 传空串等价于什么都不做，方便 `msg.show(cond ? "…" : "")` 这类写法 */
  show: (kind: MessageKind, text: string) => void;
};

const Ctx = createContext<MessageApi | null>(null);

export function useMessage(): MessageApi {
  const api = useContext(Ctx);
  if (!api) throw new Error(tr("useMessage 必须在 <MessageProvider> 内使用"));
  return api;
}

export function MessageProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<MessageItem[]>([]);
  const seq = useRef(0);

  const dismiss = useCallback((id: number) => {
    setItems((prev) => prev.filter((m) => m.id !== id));
  }, []);

  const show = useCallback((kind: MessageKind, text: string) => {
    // 后端消息（Rust 的 Err(String)）在这里统一过一遍翻译表：所有调用点都写
    // `msg.error(String(e))`，把翻译放在唯一的出口比在每个调用点包一层可靠
    const translated = trMsg(text).trim();
    if (!translated) return;
    const id = ++seq.current;
    setItems((prev) => [...prev, { id, kind, text: translated }].slice(-MAX_STACK));
  }, []);

  const api = useMemo<MessageApi>(
    () => ({
      show,
      success: (t: string) => show("success", t),
      error: (t: string) => show("error", t),
      info: (t: string) => show("info", t),
    }),
    [show],
  );

  return (
    <Ctx.Provider value={api}>
      {children}
      <div className="pointer-events-none fixed right-5 top-5 z-[100] flex flex-col items-end gap-2">
        {items.map((m) => (
          <Toast key={m.id} item={m} onDismiss={() => dismiss(m.id)} />
        ))}
      </div>
    </Ctx.Provider>
  );
}

const TONE: Record<MessageKind, { bar: string; icon: string }> = {
  success: { bar: "bg-green-500", icon: "text-green-500" },
  error: { bar: "bg-red-500", icon: "text-red-500" },
  info: { bar: "bg-[var(--accent-strong)]", icon: "text-[var(--accent-strong)]" },
};

function Toast({ item, onDismiss }: { item: MessageItem; onDismiss: () => void }) {
  const [leaving, setLeaving] = useState(false);
  const [entered, setEntered] = useState(false);

  // 退场：先播动画再真正移除，避免元素瞬间消失
  const close = useCallback(() => {
    setLeaving(true);
    window.setTimeout(onDismiss, 180);
  }, [onDismiss]);

  useEffect(() => {
    // 下一帧再切到入场终态，保证 transition 能被触发。
    // 所有类型（含错误）3s 自动消失，鼠标悬停可看，也能点 × 立即关
    const raf = requestAnimationFrame(() => setEntered(true));
    const timer = window.setTimeout(close, AUTO_DISMISS_MS);
    return () => {
      cancelAnimationFrame(raf);
      window.clearTimeout(timer);
    };
  }, [item.kind, close]);

  const tone = TONE[item.kind];

  return (
    <div
      role="status"
      className={`pointer-events-auto flex max-w-[380px] items-start gap-2.5 overflow-hidden rounded-xl border border-[var(--separator)] bg-[var(--card)] pr-3 shadow-[var(--shadow)] backdrop-blur-xl transition-all duration-200 ${
        entered && !leaving
          ? "translate-x-0 opacity-100"
          : "translate-x-3 opacity-0"
      }`}
    >
      {/* 左侧色条：不用 emoji，跟随主题的语义色 */}
      <span className={`w-[3px] self-stretch ${tone.bar}`} />
      <span className={`mt-[9px] shrink-0 ${tone.icon}`}>
        <ToastIcon kind={item.kind} />
      </span>
      <p className="py-2 text-[12.5px] leading-relaxed break-words text-[var(--text-1)]">
        {item.text}
      </p>
      <button
        className="mt-[7px] shrink-0 rounded p-0.5 text-[var(--text-2)] transition-colors hover:text-[var(--text-1)]"
        onClick={close}
        aria-label={tr("关闭")}
      >
        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round">
          <path d="M6 6l12 12M18 6L6 18" />
        </svg>
      </button>
    </div>
  );
}

function ToastIcon({ kind }: { kind: MessageKind }) {
  const common = {
    width: 15,
    height: 15,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 2,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };
  if (kind === "success") {
    return (
      <svg {...common}>
        <circle cx="12" cy="12" r="9" />
        <path d="M8.5 12.5l2.5 2.5 4.5-5" />
      </svg>
    );
  }
  if (kind === "error") {
    return (
      <svg {...common}>
        <circle cx="12" cy="12" r="9" />
        <path d="M12 7.5v5.5M12 16.5v.01" />
      </svg>
    );
  }
  return (
    <svg {...common}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 16.5V11M12 7.5v.01" />
    </svg>
  );
}
