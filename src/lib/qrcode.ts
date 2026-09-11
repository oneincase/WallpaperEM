// QR 矩阵生成：对 vendored qrcode-generator 的一层薄封装。
// 返回 boolean[][]（true = 深色模块），交给 canvas 渲染——不走 <img>/SVG，
// 避免把 challenge URL 塞进 DOM 属性被无意泄露/截屏识别之外的第三方处理
import qrcode from "../vendor/qrcode";

/**
 * 生成 QR 矩阵。typeNumber=0 表示按内容长度自动选版本；
 * 容错级别 M（15%）：屏幕显示无污损，不需要更高的 H
 */
export function qrMatrix(text: string): boolean[][] | null {
  try {
    const qr = qrcode(0, "M");
    qr.addData(text);
    qr.make();
    const n = qr.getModuleCount();
    const rows: boolean[][] = [];
    for (let r = 0; r < n; r++) {
      const row: boolean[] = [];
      for (let c = 0; c < n; c++) row.push(qr.isDark(r, c));
      rows.push(row);
    }
    return rows;
  } catch {
    // 内容过长等异常：返回 null 让调用方显示错误而不是崩掉
    return null;
  }
}
