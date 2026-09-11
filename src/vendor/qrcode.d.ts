// qrcode-generator (Kazuhiko Arase, MIT) 的类型声明。
// 只声明我们用到的最小接口：构造 → addData → make → 逐格读取深浅
interface QrCodeInstance {
  addData(data: string): void;
  make(): void;
  /** 矩阵边长（含定位块，不含留白边） */
  getModuleCount(): number;
  isDark(row: number, col: number): boolean;
}
declare function qrcode(
  typeNumber: number,
  errorCorrectionLevel: "L" | "M" | "Q" | "H",
): QrCodeInstance;
export default qrcode;
