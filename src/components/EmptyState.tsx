// 空状态占位：插图 + 标题 + 说明
//
// 替代原先一行灰字（「暂无下载任务 —— 在工坊详情页点击「下载」入队」）：
// 纯文字的空页面显得像出错了，加一张插图能明确传达「这里本来就是空的，
// 不是加载失败」，并把引导动作单独排一行。
//
// 插图统一用内联 SVG：跟随主题变色、零体积、不依赖外部资源。
// 描边风格与 icons.tsx 一致（圆头圆角），只是尺寸放大、加了柔和底色。
import type { ReactNode } from "react";

type EmptyArt =
  | "download"
  | "library"
  | "favorite"
  | "search"
  | "props";

export function EmptyState({
  art,
  title,
  hint,
  action,
}: {
  art: EmptyArt;
  title: string;
  /** 引导文案，单独一行小字 */
  hint?: string;
  action?: ReactNode;
}) {
  return (
    <div className="flex flex-col items-center justify-center px-6 py-14 text-center">
      <div className="text-[var(--text-2)]/45">{ART[art]}</div>
      <p className="mt-4 text-[14px] font-medium text-[var(--text-1)]">{title}</p>
      {hint && (
        <p className="mt-1.5 max-w-[300px] text-[12.5px] leading-relaxed text-[var(--text-2)]">
          {hint}
        </p>
      )}
      {action && <div className="mt-4">{action}</div>}
    </div>
  );
}

/** 插图统一画布：104×104，描边继承 currentColor */
function Art({ children }: { children: ReactNode }) {
  return (
    <svg
      width={104}
      height={104}
      viewBox="0 0 104 104"
      fill="none"
      stroke="currentColor"
      strokeWidth={2.6}
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {children}
    </svg>
  );
}

/** 四角星点缀，和图标集里的 Sparkle 同款语言 */
const Star = ({ x, y, r }: { x: number; y: number; r: number }) => (
  <path
    d={`M${x} ${y - r}Q${x + r * 0.24} ${y - r * 0.24} ${x + r} ${y}Q${x + r * 0.24} ${y + r * 0.24} ${x} ${y + r}Q${x - r * 0.24} ${y + r * 0.24} ${x - r} ${y}Q${x - r * 0.24} ${y - r * 0.24} ${x} ${y - r}Z`}
    fill="currentColor"
    stroke="none"
  />
);

/** 睡觉的小云朵：闭眼 + Z Z，用于「暂无任务」这类闲置态 */
const SleepyCloud = (
  <Art>
    {/* 云体：左小圆 + 顶大圆 + 右中圆 + 平底，中心约在 (50,52) */}
    <path d="M30 68a13 13 0 0 1-1.6-25.9A18 18 0 0 1 62 37.4 12 12 0 0 1 74.5 68Z" />
    {/* 闭着的眼睛：以云体中心为基准左右对称 */}
    <path d="M40 55a4.6 4.6 0 0 0 7.2 0" strokeWidth={2.3} />
    <path d="M56.8 55a4.6 4.6 0 0 0 7.2 0" strokeWidth={2.3} />
    {/* 腮红：贴着两眼外侧 */}
    <path d="M34.6 60.5h3.2M66.2 60.5h3.2" strokeWidth={2.3} />
    {/* Z Z：从云体右上角向外飘 */}
    <path d="M78 33h8l-8 9.5h8" strokeWidth={2.3} />
    <path d="M89 19h6l-6 7h6" strokeWidth={2} />
    <Star x={21} y={30} r={3} />
  </Art>
);

/** 空相框：用于本地库为空 */
const EmptyFrame = (
  <Art>
    <rect x="16" y="22" width="72" height="58" rx="10" />
    <circle cx="36" cy="42" r="5" />
    <path d="M19 68l16-15a7 7 0 0 1 9.6 0L57 64" />
    <path d="M59 66l9-8.4a7 7 0 0 1 9.6 0L85 64" />
    <Star x={90} y={18} r={4} />
    <Star x={14} y={88} r={3} />
  </Art>
);

/** 空心：用于收藏为空 */
const EmptyHeart = (
  <Art>
    <path d="M52 82c-1.6 0-3.2-.6-4.4-1.7C33.4 68 18 55 18 39.6A18.4 18.4 0 0 1 52 29.2a18.4 18.4 0 0 1 34 10.4c0 15.4-15.4 28.4-29.6 40.7A6.6 6.6 0 0 1 52 82Z" />
    <path d="M33 39a10.4 10.4 0 0 1 7.6-8.8" strokeWidth={2.2} />
    <Star x={88} y={22} r={4} />
    <Star x={17} y={72} r={3} />
  </Art>
);

/** 放大镜 + 星：用于搜索无结果 */
const EmptySearch = (
  <Art>
    <circle cx="46" cy="46" r="24" />
    <path d="M63.5 63.5 82 82" />
    {/* 镜片里的「找不到」表情 */}
    <path d="M38 40.5a3.6 3.6 0 0 1 5.4 0" strokeWidth={2.2} />
    <path d="M50.6 40.5a3.6 3.6 0 0 1 5.4 0" strokeWidth={2.2} />
    <path d="M42 55a7 7 0 0 1 10 0" strokeWidth={2.2} />
    <Star x={84} y={26} r={3.6} />
  </Art>
);

/** 滑杆 + 星：用于「无可配置项」 */
const EmptySliders = (
  <Art>
    <rect x="18" y="24" width="68" height="56" rx="12" />
    <path d="M30 44h14" strokeWidth={2.4} />
    <circle cx="50" cy="44" r="6" strokeWidth={2.4} />
    <path d="M56 44h18" strokeWidth={2.4} />
    <path d="M30 62h30" strokeWidth={2.4} />
    <circle cx="66" cy="62" r="6" strokeWidth={2.4} />
    <path d="M72 62h2" strokeWidth={2.4} />
  </Art>
);

const ART: Record<EmptyArt, ReactNode> = {
  download: SleepyCloud,
  library: EmptyFrame,
  favorite: EmptyHeart,
  search: EmptySearch,
  props: EmptySliders,
};
