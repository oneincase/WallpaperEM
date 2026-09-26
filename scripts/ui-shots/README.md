# 界面截图工具 / UI screenshot tool

抓 README（`## 🖼️ 界面截图`）与商店页要用的主界面截图：**按窗口** 2x 抓拍，
输出与现有图一致（2480×1602、圆角透明像素压到玻璃边缘色的 JPEG q88）。

```bash
./shoot.sh                      # 抓页面表里的全部页 → <repo>/.ui-shots/raw/*.png
./shoot.sh home library         # 只抓指定页（想重拍某一页时用）
./shoot.sh --emit home shares   # 抓完直接压成 JPEG 落 docs/img/shot-<key>.jpg
./shoot.sh --list               # 打印页面表（key / 导航 / 标签 / 等待）
```

依赖：macOS + Xcode CLT（`swiftc`）+ `python3` 与 Pillow；应用最好正以
`pnpm tauri dev` 跑着（脚本会抓运行中的实例；主窗口关着也会自己唤起）。
工作目录默认 `<repo>/.ui-shots/`（已 gitignore），用 `UI_SHOTS_WORK` 可换。

## 文件

| 文件 | 作用 |
| --- | --- |
| `shoot.sh` | 编排：编译 helper → 确保主窗口在屏 → 逐页切页/抓图/体检 →（可选）压成 JPEG |
| `axdrive.swift` | AX 驱动：`tree` 打树、`list` 列按钮、`press <标题>` 按标题点按钮（切页不用鼠标） |
| `windowlist.swift` | 列本应用窗口 / 取「在屏主窗口」id（`screencapture -l <id>` 用） |
| `pipeline.py` | 抓图 → README 用 JPEG（压平透明 + 质量压缩） |
| `verify.py` | 抓图体检（尺寸 / 是否空白 / 亮度），可顺手出缩略图 |

## 几个必须知道的坑

- **切页别用坐标点击**：顶部胶囊导航选中/悬浮会横向拉长（`index.css` 的
  `grid 0fr→1fr`），点下去的位置在点的那一刻已经变了。`axdrive press` 按 AX 标题点，
  既稳又不动指针、不抢前台应用。
- **同名文案要限定纵向范围**：`shoot.sh` 给导航条的 press 传了窗口顶部 y 范围
  （`导航条在窗口 y+4..y+52`）。正文里也有「分享」「关于」之类的字，不限定会点错。
- **抓图要求窗口在屏**：最小化 / 别的 Space 时 `screencapture -l` 不报错，只给一张
  空窗图。所以每张都跑 `verify.py`，`std≈0` 就是没抓到。跨 Space 时把窗口拖回当前
  Space 再抓。
- **透明圆角自己压平**：窗口是透明 + CSS 圆角的，`sips` 转 JPEG 会把圆角外压成白色
  （深色玻璃 UI 上一眼假）。`pipeline.py` 取左缘中点像素当底色再压平。
- **「关于」页的等待设为 0**：该页一进去就自动查更新清单，发版前清单不存在会显示
  红字报错，所以按下即抓（拍到中性态）。发版后可以把等待改成 2500ms 抓「已是最新版本」。
- **`pnpm tauri dev` 常驻时慎跑 `pnpm run build`**：两者共用 vite 缓存，实测会让 dev
  会话干净退出（与本工具无关，但截图时容易撞上）。

## 加一页 / 改一页

在 `shoot.sh` 顶部的 `SHOTS` 表加一行 `"key|导航标题|设置内标签|等待ms"`：

- `key` 决定 `--emit` 的输出名（`docs/img/shot-<key>.jpg`）——改了要同步 README 的
  `<img src>` 与图注；
- 导航标题就是胶囊上的字（发现 / 工坊 / 下载 / 本地库 / 收藏 / 显示器 / 分享 /
  快捷键 / 设置）；设置类页面再给一个标签名（账号 / 通用 / 画质 / 网络与服务 / 关于）。
- 不知道按钮在 AX 里叫什么、在哪一行时：`pgrep -x WallpaperEM` 拿 pid，然后
  `./.bin/axdrive <pid> tree 6` 或 `list <yMin> <yMax>` 看一眼。
