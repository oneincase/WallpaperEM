// 二维码 canvas 渲染组件：白底 + 4 格静区（quiet zone，扫码识别的硬性要求），
// 深色模块用接近纯黑的颜色。image-rendering: pixelated 保证缩放后边缘锐利
import { useEffect, useRef } from "react";
import { tr } from "../lib/i18n";
import { qrMatrix } from "../lib/qrcode";

const QUIET_ZONE = 4;

export function QrImage({
  text,
  /** 组件边长（px，CSS 尺寸；内部按 devicePixelRatio 高清渲染） */
  size = 180,
}: {
  text: string;
  size?: number;
}) {
  const ref = useRef<HTMLCanvasElement>(null);
  const matrix = qrMatrix(text);

  useEffect(() => {
    const canvas = ref.current;
    if (!canvas || !matrix) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = size * dpr;
    canvas.height = size * dpr;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.scale(dpr, dpr);
    ctx.fillStyle = "#ffffff";
    ctx.fillRect(0, 0, size, size);
    const n = matrix.length;
    const cell = size / (n + QUIET_ZONE * 2);
    ctx.fillStyle = "#000000";
    for (let r = 0; r < n; r++) {
      for (let c = 0; c < n; c++) {
        if (matrix[r][c]) {
          ctx.fillRect((c + QUIET_ZONE) * cell, (r + QUIET_ZONE) * cell, cell, cell);
        }
      }
    }
  }, [matrix, size]);

  if (!matrix) {
    return (
      <div
        className="flex items-center justify-center rounded-xl bg-white text-[12px] text-red-500"
        style={{ width: size, height: size }}
      >
        {tr("二维码生成失败")}
      </div>
    );
  }
  return (
    <canvas
      ref={ref}
      className="rounded-xl shadow-sm [image-rendering:pixelated]"
      style={{ width: size, height: size }}
    />
  );
}
