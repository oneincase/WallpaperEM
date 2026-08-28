// 轻量 SF 风格线性图标集（内联 SVG，stroke 随 currentColor）
import type { ReactNode } from "react";

function Svg({ children, size = 17 }: { children: ReactNode; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ flexShrink: 0 }}
    >
      {children}
    </svg>
  );
}

export const IconHome = () => (
  <Svg>
    <path d="M3 10.5 12 3l9 7.5" />
    <path d="M5 9.5V21h5v-6h4v6h5V9.5" />
  </Svg>
);

export const IconGrid = () => (
  <Svg>
    <rect x="3" y="3" width="7.5" height="7.5" rx="1.5" />
    <rect x="13.5" y="3" width="7.5" height="7.5" rx="1.5" />
    <rect x="3" y="13.5" width="7.5" height="7.5" rx="1.5" />
    <rect x="13.5" y="13.5" width="7.5" height="7.5" rx="1.5" />
  </Svg>
);

export const IconDownload = () => (
  <Svg>
    <path d="M12 3v12" />
    <path d="m7 11 5 5 5-5" />
    <path d="M4 19h16" />
  </Svg>
);

export const IconLibrary = () => (
  <Svg>
    <rect x="3" y="4" width="18" height="16" rx="2" />
    <path d="M3 9h18" />
    <path d="M8 14h8" />
  </Svg>
);

export const IconHeart = () => (
  <Svg>
    <path d="M12 20.5S4 15.5 4 9.8A4.3 4.3 0 0 1 12 7a4.3 4.3 0 0 1 8 2.8c0 5.7-8 10.7-8 10.7Z" />
  </Svg>
);

export const IconGear = () => (
  <Svg>
    <circle cx="12" cy="12" r="3.2" />
    <path d="M19 12a7 7 0 0 0-.14-1.4l2-1.55-2-3.46-2.36.94a7 7 0 0 0-2.42-1.4L13.6 2.6h-4l-.48 2.53a7 7 0 0 0-2.42 1.4l-2.36-.94-2 3.46 2 1.55a7 7 0 0 0 0 2.8l-2 1.55 2 3.46 2.36-.94a7 7 0 0 0 2.42 1.4l.48 2.53h4l.48-2.53a7 7 0 0 0 2.42-1.4l2.36.94 2-3.46-2-1.55a7 7 0 0 0 .14-1.4Z" />
  </Svg>
);

export const IconPlay = ({ size }: { size?: number }) => (
  <Svg size={size}>
    <path d="M7 4.5v15l12-7.5-12-7.5Z" fill="currentColor" stroke="none" />
  </Svg>
);

export const IconPause = ({ size }: { size?: number }) => (
  <Svg size={size}>
    <path d="M8 5v14M16 5v14" strokeWidth={2.4} />
  </Svg>
);

export const IconStop = ({ size }: { size?: number }) => (
  <Svg size={size}>
    <rect x="5.5" y="5.5" width="13" height="13" rx="2" fill="currentColor" stroke="none" />
  </Svg>
);

// 侧边栏收缩 / 展开（分栏图标）
export const IconSidebarCollapse = () => (
  <svg
    width={17}
    height={17}
    viewBox="0 0 24 24"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    style={{ flexShrink: 0 }}
  >
    <rect x="1.5" y="1.5" width="21" height="21" rx="4" stroke="currentColor" strokeWidth="2" />
    <line x1="8" y1="2" x2="8" y2="22" stroke="currentColor" strokeWidth="2" />
  </svg>
);

export const IconSidebarExpand = IconSidebarCollapse;
