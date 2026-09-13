// 圆润可爱风线性图标集（内联 SVG，stroke 随 currentColor）
//
// 相比通用 SF 线性图标做了三件事，让它更「二次元」而不失克制：
//   1. 描边加粗到 2.1px、端点与拐角全圆角 —— 线条更软，去掉工程感；
//   2. 形状整体饼度化：矩形圆角加大、直角改弧线、比例更矮胖；
//   3. 关键图标点缀星星 / 心形 / 高光等小装饰（用更细的描边，不抢主体）。
//
// 仍保持 currentColor + 24 viewBox + 同样的导出名，所以调用方一行不用改，
// 也能继续跟随主题明暗与 hover 变色。
import type { ReactNode } from "react";

function Svg({
  children,
  size = 17,
  width,
}: {
  children: ReactNode;
  size?: number;
  /** 覆盖描边粗细；装饰性细节可传更小的值 */
  width?: number;
}) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={width ?? 2.1}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ flexShrink: 0 }}
    >
      {children}
    </svg>
  );
}

/** 四角星装饰：二次元图标里最常见的「闪」，用细描边避免抢主体 */
const Sparkle = ({ x, y, r = 1.9 }: { x: number; y: number; r?: number }) => (
  <path
    d={`M${x} ${y - r}Q${x + r * 0.22} ${y - r * 0.22} ${x + r} ${y}Q${x + r * 0.22} ${y + r * 0.22} ${x} ${y + r}Q${x - r * 0.22} ${y + r * 0.22} ${x - r} ${y}Q${x - r * 0.22} ${y - r * 0.22} ${x} ${y - r}Z`}
    fill="currentColor"
    stroke="none"
  />
);

// ---------- 侧边栏导航 ----------

/** 发现：圆顶小屋 + 一颗星 */
export const IconHome = () => (
  <Svg>
    <path d="M4 11.2c0-.6.26-1.16.7-1.55l6-5.3a2 2 0 0 1 2.6 0l6 5.3c.44.39.7.95.7 1.55V18a2.6 2.6 0 0 1-2.6 2.6H6.6A2.6 2.6 0 0 1 4 18Z" />
    <path d="M9.7 20.6v-4.3a2.3 2.3 0 0 1 4.6 0v4.3" />
  </Svg>
);

/** 工坊：四格圆角方块，右上角一颗星 */
export const IconGrid = () => (
  <Svg>
    <rect x="3.2" y="3.2" width="7.4" height="7.4" rx="2.6" />
    <rect x="3.2" y="13.4" width="7.4" height="7.4" rx="2.6" />
    <rect x="13.4" y="13.4" width="7.4" height="7.4" rx="2.6" />
    <path d="M17.1 3.4a3.7 3.7 0 0 0 3.5 3.5 3.7 3.7 0 0 0-3.5 3.5 3.7 3.7 0 0 0-3.5-3.5 3.7 3.7 0 0 0 3.5-3.5Z" />
  </Svg>
);

/** 下载：胖箭头 + 托盘 */
export const IconDownload = () => (
  <Svg>
    <path d="M12 3.6v10.2" />
    <path d="M7.6 10.2 12 14.4l4.4-4.2" />
    <path d="M4.4 17.2v1.4a2 2 0 0 0 2 2h11.2a2 2 0 0 0 2-2v-1.4" />
  </Svg>
);

/** 本地库：圆角相册 + 小山与太阳 */
export const IconLibrary = () => (
  <Svg>
    <rect x="3" y="4.4" width="18" height="15.2" rx="3.4" />
    <circle cx="8.6" cy="9.6" r="1.5" />
    <path d="M3.6 16.6l4-3.6a1.8 1.8 0 0 1 2.4 0l3 2.7" />
    <path d="M13.6 17.4l2.5-2.3a1.8 1.8 0 0 1 2.4 0l1.9 1.7" />
  </Svg>
);

/** 收藏：饱满的心 + 高光 */
export const IconHeart = () => (
  <Svg>
    <path d="M12 20.4c-.4 0-.8-.14-1.1-.42C7.4 16.9 3.6 13.6 3.6 9.8A4.6 4.6 0 0 1 12 7.2a4.6 4.6 0 0 1 8.4 2.6c0 3.8-3.8 7.1-7.3 10.18-.3.28-.7.42-1.1.42Z" />
    <path d="M7.4 9.5a2.6 2.6 0 0 1 1.9-2.2" strokeWidth={1.3} />
  </Svg>
);

// ---------- 本地库卡片操作：预览 / 应用 / 打开文件 / 删除 ----------

/** 预览：圆眼睛 + 眼底高光 */
export const IconPreview = () => (
  <Svg>
    <path d="M2.8 12s3.6-6.2 9.2-6.2S21.2 12 21.2 12s-3.6 6.2-9.2 6.2S2.8 12 2.8 12Z" />
    <circle cx="12" cy="12" r="3.2" />
    <circle cx="13.3" cy="10.8" r=".85" fill="currentColor" stroke="none" />
  </Svg>
);

/** 应用到桌面：圆角显示器 + 底座 */
export const IconApply = () => (
  <Svg>
    <rect x="2.8" y="4.2" width="18.4" height="12.4" rx="3.2" />
    <path d="M9.2 20.4h5.6" />
    <path d="M12 16.6v3.8" />
  </Svg>
);

/** 打开目录：圆角文件夹 + 翘起的封口 */
export const IconOpenFile = () => (
  <Svg>
    <path d="M3 7.6a2.6 2.6 0 0 1 2.6-2.6h3.3c.62 0 1.2.28 1.6.76l1.1 1.34h6.8A2.6 2.6 0 0 1 21 9.7v7.7a2.6 2.6 0 0 1-2.6 2.6H5.6A2.6 2.6 0 0 1 3 17.4Z" />
    <path d="M3.3 11.4h17.4" strokeWidth={1.5} />
  </Svg>
);

/** 上传到创意工坊：托盘 + 向上箭头 */
export const IconUpload = () => (
  <Svg>
    <path d="M4.4 15.2v2.4a2.4 2.4 0 0 0 2.4 2.4h10.4a2.4 2.4 0 0 0 2.4-2.4v-2.4" />
    <path d="M12 15.6V4.4" />
    <path d="M7.6 8.4 12 4l4.4 4.4" />
  </Svg>
);

/** 删除：圆角垃圾桶，桶身两道短竖 */
export const IconTrash = () => (
  <Svg>
    <path d="M4.4 7.2h15.2" />
    <path d="M9.4 7.2V5.9A2 2 0 0 1 11.4 3.9h1.2a2 2 0 0 1 2 2v1.3" />
    <path d="M6.4 7.2l.72 10.5a2.6 2.6 0 0 0 2.6 2.4h4.56a2.6 2.6 0 0 0 2.6-2.4l.72-10.5" />
    <path d="M10.4 11.4v5M13.6 11.4v5" strokeWidth={1.6} />
  </Svg>
);

/** 设置：花瓣状齿轮（比标准齿轮更圆润） */
export const IconGear = () => (
  <Svg>
    <circle cx="12" cy="12" r="3.1" />
    <path d="M12 3.4a1.9 1.9 0 0 1 1.86 1.53l.16.8a7 7 0 0 1 1.5.87l.77-.28a1.9 1.9 0 0 1 2.24.86l.5.87a1.9 1.9 0 0 1-.38 2.36l-.6.55a7 7 0 0 1 0 1.74l.6.55a1.9 1.9 0 0 1 .38 2.36l-.5.87a1.9 1.9 0 0 1-2.24.86l-.77-.28a7 7 0 0 1-1.5.87l-.16.8a1.9 1.9 0 0 1-1.86 1.53h-1a1.9 1.9 0 0 1-1.86-1.53l-.16-.8a7 7 0 0 1-1.5-.87l-.77.28a1.9 1.9 0 0 1-2.24-.86l-.5-.87a1.9 1.9 0 0 1 .38-2.36l.6-.55a7 7 0 0 1 0-1.74l-.6-.55a1.9 1.9 0 0 1-.38-2.36l.5-.87a1.9 1.9 0 0 1 2.24-.86l.77.28a7 7 0 0 1 1.5-.87l.16-.8A1.9 1.9 0 0 1 11 3.4Z" />
  </Svg>
);

/** 自定义属性：双滑杆，滑块是圆润的胶囊 */
export const IconSliders = ({ size }: { size?: number }) => (
  <Svg size={size}>
    <path d="M4 7.4h7.2" />
    <rect x="11.2" y="4.6" width="5.2" height="5.6" rx="2.8" />
    <path d="M16.4 7.4H20" />
    <path d="M4 16.6h3.4" />
    <rect x="7.4" y="13.8" width="5.2" height="5.6" rx="2.8" />
    <path d="M12.6 16.6H20" />
  </Svg>
);

// ---------- 播放控制 ----------

/** 播放：圆角三角 */
export const IconPlay = ({ size }: { size?: number }) => (
  <Svg size={size}>
    <path
      d="M8.2 5.6a1.4 1.4 0 0 1 2.1-1.22l8 6.4a1.4 1.4 0 0 1 0 2.44l-8 6.4a1.4 1.4 0 0 1-2.1-1.22Z"
      fill="currentColor"
      stroke="currentColor"
      strokeWidth={1.4}
    />
  </Svg>
);

/** 暂停：两根圆头胶囊 */
export const IconPause = ({ size }: { size?: number }) => (
  <Svg size={size}>
    <rect x="6.6" y="4.8" width="3.8" height="14.4" rx="1.9" fill="currentColor" stroke="none" />
    <rect x="13.6" y="4.8" width="3.8" height="14.4" rx="1.9" fill="currentColor" stroke="none" />
  </Svg>
);

/** 停止：大圆角方块 */
export const IconStop = ({ size }: { size?: number }) => (
  <Svg size={size}>
    <rect x="5.4" y="5.4" width="13.2" height="13.2" rx="3.6" fill="currentColor" stroke="none" />
  </Svg>
);

// ---------- 侧边栏收缩 / 展开 ----------

export const IconSidebarCollapse = () => (
  <Svg>
    <rect x="2.8" y="3.6" width="18.4" height="16.8" rx="4" />
    <path d="M9.4 3.8v16.4" />
  </Svg>
);

export const IconSidebarExpand = IconSidebarCollapse;

// ---------- 主题切换 ----------

/** 浅色：胖太阳 + 短光芒 */
export const IconSun = () => (
  <Svg>
    <circle cx="12" cy="12" r="4.4" />
    <path d="M12 2.9v1.9M12 19.2v1.9M4.6 4.6l1.35 1.35M18.05 18.05l1.35 1.35M2.9 12h1.9M19.2 12h1.9M4.6 19.4l1.35-1.35M18.05 5.95 19.4 4.6" />
  </Svg>
);

/** 深色：月牙 + 两颗星 */
export const IconMoon = () => (
  <Svg>
    <path d="M20.2 14.6A8.4 8.4 0 0 1 9.4 3.8a8.4 8.4 0 1 0 10.8 10.8Z" />
    <Sparkle x={17.4} y={5.6} r={1.7} />
    <Sparkle x={20.4} y={9.4} r={1.1} />
  </Svg>
);

/** 跟随系统：左半太阳右半月 */
export const IconAuto = () => (
  <Svg>
    <circle cx="12" cy="12" r="4.4" />
    <path d="M12 7.6a4.4 4.4 0 0 1 0 8.8Z" fill="currentColor" stroke="none" />
    <path d="M12 2.9v1.9M12 19.2v1.9M4.6 4.6l1.35 1.35M2.9 12h1.9M4.6 19.4l1.35-1.35" />
  </Svg>
);
