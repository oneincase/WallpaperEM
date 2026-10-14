# WallpaperEM 的 Plasma 壁纸插件（打包进安装包的副本）

> **不要在这里改代码。** 这里的内容是**单向同步过来的副本**：
> 插件的主体在同级独立仓库 **`WallpaperEM-Plasma`**（与主仓同级目录，
> https://github.com/oneincase/WallpaperEM-Plasma ），改完在那边提交，再跑
> ```bash
> bash scripts/sync-plasma-plugin.sh            # 默认取 ../WallpaperEM-Plasma
> ```
> 把 `metadata.json` + `contents/` 搬进来（本文件与独立仓的 README 不同，脚本不会覆盖它）。

为什么仓库里要留一份副本：应用内「一键启用 KDE 插件」
（设置 → 通用 → KDE 桌面集成，`src-tauri/src/kde_plugin.rs`）按的是**随安装包分发**的
那份包 —— `src-tauri/tauri.conf.json` 的 `bundle.resources` 把
`../packaging/plasma-wallpaper` 映射成 `$RESOURCE/plasma-wallpaper`，运行时
`kde_plugin::resolve_package_dir()` 从这里找包，再拷进
`~/.local/share/plasma/wallpapers/io.github.oneincase.wallpaperem/`。
也就是说：**这份副本不同步，用户点「启用」装上的就是旧插件。**

## 为什么必须是插件（而不是引擎自己开窗口）

外部窗口方案已被真机实测穷尽证伪：KWin 自管桌面层的层内次序，
`_NET_WM_STATE_BELOW` / `_NET_RESTACK_WINDOW` / `XConfigureWindow` / override-redirect
全都无法把窗口排到 plasmashell 桌面视图之下，而桌面图标就画在那个窗口里
（证据见 [docs/kde-adaptation-plan.md](../../../../docs/kde-adaptation-plan.md) §2.5 与
[docs/kde-verification-report.md](../../../../docs/kde-verification-report.md)）。

插件跑在 `plasmashell` 进程里、和桌面图标同属一个 containment，因此不存在
「谁盖住谁」的问题 —— 这是「壁纸 + 桌面图标同时可见」在 KDE 下的**唯一**可行路径。

## 这份副本包含什么

```
io.github.oneincase.wallpaperem/
├── metadata.json            # KPlugin 元数据（ServiceTypes=["Plasma/Wallpaper"]，Id 见上）
└── contents/
    ├── config/main.xml      # KConfigXT：10 个配置项（FramePath/SeqPath/UseSeqFile/…）
    └── ui/
        ├── main.qml         # 纯显示端：双缓冲贴帧、序号轮询、停帧提示、兜底色/图
        └── config.qml       # 配置对话框（cfg_<Key> 别名绑定）
```

## 状态

| 能力 | 状态 |
| --- | --- |
| 独立仓 `scripts/verify.sh`（Xvfb + qmlscene，16 项） | ✅ 通过（Plasma 5.27.12） |
| 独立仓 `scripts/plasma-real-test.sh`（真 plasmashell + 真桌面，18 项） | ✅ 通过：与 `org.kde.image` 铺同一张图逐像素一致，桌面图标/面板完好 |
| 引擎侧帧导出 → 插件显示（端到端，19 项） | ✅ 通过（`scripts/kde-frame-export-check.sh`，`hideWindow` + `windowCapture`） |
| 应用内一键启用/关闭（本仓 `kde_plugin`） | ✅ 已实现，走 MCP 在真机验过 |

复现与细节见独立仓的 `README.md`、`docs/engine-contract.md`
（帧契约全文 + §9 引擎侧实现状态）与 `docs/real-plasma-verification.md`（证据图）。
