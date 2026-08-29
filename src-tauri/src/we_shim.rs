//! WE 兼容 shim 脚本本体（内嵌 we_shim.js，由内容服务器注入库内壁纸 HTML）。
//! 单独成模块以便 `mod we_shim;` 注册与 include_str! 引用。

pub const SRC: &str = include_str!("we_shim.js");
