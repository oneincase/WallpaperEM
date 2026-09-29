//! 插件系统（宿主侧）。
//!
//! 本应用的「插件」对外只有一个动作：**打开一扇属于它的窗口**。窗口里装什么由
//! 插件自己决定 —— 内置插件装的是本应用自己接管的界面（当前只有 DeepSeek
//! Harness，见 [`dsh`]），第三方插件装的是外部页面。
//!
//! 模块划分：
//! - [`dsh`]：内置插件 DeepSeek Harness —— 环境扫描 / 干净 profile 准备 /
//!   嵌入本应用 MCP / 启动并把它的 Web GUI 开成应用窗口；
//! - [`store`]：第三方插件的**声明式清单**存储（热插拔：装/卸/重扫都不重启应用）
//!   与市场数据源（随包官方清单 + GitHub `topic:wem-plugin` 搜索）；
//! - [`window`]：把任意 URL 开成应用窗口的公共实现。
//!
//! 状态只有一处：dsh 子进程句柄。插件窗口关掉、应用退出都必须把它收掉 ——
//! 否则用户桌面上会留下一堆看不见的 node 进程（每开一次插件一个）。

pub(crate) mod dsh;
pub(crate) mod store;
mod window;

use serde::Serialize;
use tauri::AppHandle;

/// 插件命令的错误：`code` 是稳定标识（前端据此出本地化文案），`detail` 是诊断
/// 细节（进程输出 / io 错误原文）。**刻意不带中文** —— 文案由前端 `tr()` 负责，
/// 后端只管事实。
#[derive(Debug, Serialize)]
pub struct PluginError {
    pub code: String,
    pub detail: String,
}

impl PluginError {
    pub(crate) fn new(code: &str, detail: impl std::fmt::Display) -> Self {
        Self {
            code: code.into(),
            detail: detail.to_string(),
        }
    }
    pub(crate) fn code(code: &str) -> Self {
        Self {
            code: code.into(),
            detail: String::new(),
        }
    }
}

/// 注册插件宿主状态。放在 setup 里、窗口创建之前。
pub fn init(app: &AppHandle) {
    dsh::init(app);
}

/// 退出前的收尾：dsh 子进程不能变成孤儿进程。
pub fn shutdown_all(app: &AppHandle) {
    dsh::shutdown(app);
}
