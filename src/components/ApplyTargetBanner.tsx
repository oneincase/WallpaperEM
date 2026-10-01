// 「应用目标已锁定」全局横幅：显示器坞锁定某块屏后，下一次点「应用」只落那一屏。
// 必须跟着全局走 —— 只在库页提示过的话，工坊/详情页点「应用」会看到只有
// 一块屏变了却毫无解释（armed 是模块级全局状态，消费它的动作散布在三个页面）。
import { cancelApplyTarget, useArmedApplyTarget } from "../lib/apply-target";
import { tr } from "../lib/i18n";

export function ApplyTargetBanner() {
  const armed = useArmedApplyTarget();
  if (!armed) return null;
  return (
    <div className="pointer-events-auto fixed left-1/2 top-[60px] z-40 flex -translate-x-1/2 items-center gap-2 rounded-full border border-[var(--accent-strong)]/40 bg-[var(--accent)]/15 px-3 py-1.5 text-[12.5px] shadow-lg backdrop-blur">
      <span className="truncate">
        {tr("正在为「{name}」选择壁纸 —— 点「应用」只设置该屏", { name: armed.name })}
      </span>
      <button className="btn !py-0.5 text-[11.5px]" onClick={cancelApplyTarget}>
        {tr("取消")}
      </button>
    </div>
  );
}
