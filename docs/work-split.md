# 分工与交接

这个仓库有两双手在推：**后端 / 工具链那条线**（本机 Agent 协助）与**页面 / 交互那条线**
（fork `elysia395`）。这份文件只说一件事：**哪一块归谁**，以及**哪些结构已经占了位** ——
避免重复劳动，或把新结构改回旧样子。

规则（怎么写提交、要不要开 PR）仍然只有一个来源：`docs/pr-rules.md`。

## 归后端 / 工具链

- `src-tauri/**`：MCP 工具链（`src-tauri/src/mcp/*`）、壁纸引擎（`wallpaper/*`）、
  `content_server`、`workspace`（工程 / 打包）、平台层（`system_wallpaper` / `blur` / `hotkeys`）
- `scripts/**`、`.github/workflows/**`、`docs/pr-rules.md`
- `proto-video-loop/**`（原型）

## 归页面 / 交互

- `src/pages/**`、`src/components/**`、`src/App.tsx`、`src/props-main.tsx`、`src/index.css`
- 交互流程：应用菜单与目标选择、预览、抽屉 / 遮罩层级、分享页
- 实机验证与截图（`docs/img/**`）

## 两边都会碰的（约定：先落的人保留，后落的人 rebase 一次）

- `src/hooks/useApplyWallpaper.tsx` —— 应用流程
- `src/locales/en-US.ts` —— 词条：两边都在加，各自追加即可，**不要重排或合并同一段**
- `CHANGELOG.md` —— `[Unreleased]` 里**各写各的小节**，不要动对方的小节
- `README.md`

## 已占位的结构（2026-09-29 落地，别按旧结构改回去）

- `src/pages/Home.tsx`、`src/pages/Displays.tsx` **已删除**：显示器管理并入**工坊页 + `DisplayDock`
  显示器坞**，壁纸详情走 `WallpaperInfoModal`；默认落地页是**工坊**。
- 新增 `src/lib/preview.ts`（应用内预览）。
- 视频壁纸改挂独立 `video-layer` 容器：`filter` / `opacity` / `transform` 不再直接作用于 `<video>`。
- 截图链路统一走**渲染器自抓帧**（`content_server::request_capture` ↔ 渲染器 `window.__wpCapture`）。

## 当前在飞

- **#10**（fork `feat/apply-menu-stop`，6 条提交）：应用目标菜单 + 「再点播放中的屏 = 停止该屏」
  + 「统一应用」主按钮 + portal 渲染修复 + 实机截图。**基线停在 `5a10c5f`**，早于上面那批 UI 落地 ——
  直接合会把 `Home` / `Displays` / `Library` / `preview.ts` / `en-US.ts` 覆盖回旧版，**需要先 rebase 到最新 `main`**。
- `backup/stash-wip-mcp-library`：后端侧在制 stash（442+/43-），归后端线。
- `wip/standby-warm-window-abandoned`：被放弃的「热备窗」方向，仅作参考。
