# WallpaperEM 的 Plasma 壁纸插件（P9）

这是「壁纸 + 桌面图标同时可见」在 KDE 下的**唯一**可行路径：插件跑在 `plasmashell`
进程里，和桌面图标同属一个 containment，因此不存在「谁盖住谁」的问题。

> 背景：外部窗口方案已被真机实测穷尽证伪——KWin 自管桌面层的层内次序，
> `_NET_WM_STATE_BELOW` / `_NET_RESTACK_WINDOW` / `XConfigureWindow` / override-redirect
> 全都无法把窗口排到 plasmashell 桌面视图之下，而桌面图标就画在那个窗口里。
> 证据见 [docs/kde-adaptation-plan.md](../../../../docs/kde-adaptation-plan.md) §2.5 与
> [docs/kde-verification-report.md](../../../../docs/kde-verification-report.md)。

## 当前状态（2026-10-14）

| 能力 | 状态 |
| --- | --- |
| KPackage 结构 / `kpackagetool5` 安装注册 | ✅ 真机验证（Plasma 5.27.12，VM `10.211.55.5`） |
| QML 把「引擎写出的帧文件」显示出来 | ✅ 真机验证（`qmlscene` + Xvfb 截图取样，帧为品红时取到 `srgb(255,0,255)`） |
| 帧文件缺失时纯色兜底、不报错 | ✅ 真机验证（取到 `#101014`） |
| 在真 plasmashell 里被选为桌面壁纸 | ⬜ 未验证（需要能正常登录的图形会话） |
| 引擎侧帧传输（DMA-BUF / socket） | ⬜ 未实现（`zwp_linux_dmabuf_v1` 在 5.27 上实测 v4 可用） |
| 每虚拟桌面独立壁纸 | ⬜ 不可行（plasmashell 本身不支持，KDE bug 341143） |

复现：`bash scripts/kde-plugin-check.sh`（11 项断言，自带 Xvfb + qmlscene，跑完自动卸载）。

## 引擎 ↔ 插件契约

插件是**纯显示端**，不做渲染、不碰网络、不写文件。

1. **帧文件**：引擎把最新帧写成 PNG 到配置项 `FramePath`（默认
   `/tmp/wallpaperem/frame.png`）。
2. **原子替换**：先写 `<FramePath>.tmp`，再 `rename()` 覆盖。<u>不要原地覆写</u>——
   插件随时可能在读，原地写会让它读到半张图（表现为撕裂/花屏）。
3. **只读**：插件永不写、删、改名该文件；引擎负责清理。
4. **不存在时的行为**：显示 `FallbackColor`（默认 `#101014`）并居中提示
   「WallpaperEM 引擎未运行」；等离子桌面不受影响。
5. **刷新**：插件按 `RefreshMs`（默认 66ms ≈ 15fps）轮询。引擎写得更快也没用，
   写得更慢就是掉帧。`RefreshMs=0` = 只读一次（静态图模式）。
6. **尺寸**：插件按 `FillMode`（0 拉伸 / 1 等比裁切 / 2 等比适应）铺满 containment。
   引擎应按 `逻辑尺寸 × devicePixelRatio` 渲染，否则会被放大糊掉。
7. **崩溃隔离**：插件在 plasmashell 进程内运行，**它崩 = 整个桌面崩**。因此这里
   不得有阻塞 IO、不得抛异常、不得引入会 abort 的 C++ 插件依赖。解码走 `Image`
   的异步路径是刻意的。
8. **每个 containment 一个实例**：多屏/多活动各有自己的插件实例，目前的配置是
   每实例独立存的，但引擎侧还没做「按实例区分帧文件」——多屏下会互相覆盖，
   属于已知缺口。

## 为什么不用「插件内跑 WebKitGTK」

渲染栈（WebKitGTK + GL + 音频可视化）已经在主应用里跑通并且要和播放列表、
音频捕获、自动暂停共享状态。把它塞进 plasmashell 进程会让桌面稳定性绑定在
整个渲染栈上（任一崩溃=桌面崩溃），也拿不到 GPU 上下文配额保证。所以插件
只做显示端，渲染留在引擎进程。

## 目录

```
io.github.oneincase.wallpaperem/
├── metadata.json            # KPlugin 元数据（ServiceTypes=["Plasma/Wallpaper"]）
└── contents/
    ├── config/main.xml      # KConfigXT：FramePath / RefreshMs / FallbackColor / FillMode
    └── ui/
        ├── main.qml         # 显示端；无 plasmashell 时用内部 stub 配置（可独立渲染）
        └── config.qml       # 配置对话框（cfg_<Key> 别名绑定）
```

## 安装（用户）

```bash
kpackagetool5 --type Plasma/Wallpaper --install packaging/plasma-wallpaper/io.github.oneincase.wallpaperem
# 然后在「桌面设置 → 壁纸 → 类型」里选 WallpaperEM
# 卸载：kpackagetool5 --type Plasma/Wallpaper --remove io.github.oneincase.wallpaperem
```

Plasma 6 用户把 `kpackagetool5` 换成 `kpackagetool6`（本目录未在 Plasma 6 上验证过，
这是本轮唯一拿不到 Plasma 6 底座造成的缺口）。
