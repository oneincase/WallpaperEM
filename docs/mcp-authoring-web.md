# 网页壁纸工程规范（MCP / `type: "web"`）

面向「用 MCP 工具做一张网页壁纸」的 agent。看完这份文档再动手：
MCP 的 `project_create(type="web")` 会落一套可直接跑通的骨架，
**改它，不要从零另起**；判据以本文件为准。

相关资源：`wallpaperem://templates/web-basic`（模板文件全文）、
prompt `create_web_wallpaper`（完整闭环流程）。

---

## 1. 交付物与目录结构

工程根：`~/Documents/WallpaperEM/Projects/<工程名>/`。典型布局：

```
<工程名>/
├── project.json        必需：类型 / 标题 / 入口 / 版本 / 标签 / 用户属性
├── index.html          入口页（也可放 web/index.html，见 §6）
├── style.css
├── main.js
├── assets/…            贴图 / 字体 / 音频等，按需
└── .version/<版本号>/   版本历史（工具自动维护，别手写）
```

- 打包/安装时 **`project.json` 之外的文件全部随工程一起拷进本地库**，
  所以相对路径引用随便用，只要别指到工程外。
- 单文件写入上限 512 MiB（base64 解码后），工程内不要塞整库大文件。

---

## 2. `project.json`

```json
{
  "type": "web",
  "title": "雨夜霓虹",
  "file": "index.html",
  "version": 1,
  "tags": ["Anime", "Everyone"],
  "general": {
    "properties": {
      "schemecolor": {
        "order": 0,
        "text": "ui_browse_properties_scheme_color",
        "type": "color",
        "value": "0.28 0.62 1"
      },
      "speed": {
        "order": 1, "text": "动画速度", "type": "slider",
        "value": 0.5, "min": 0, "max": 2, "step": 0.05, "precision": 2
      }
    }
  }
}
```

| 字段 | 必需 | 说明 |
| --- | --- | --- |
| `type` | 是 | 固定 `"web"`。缺了校验会报错；写别的类型会走别的渲染路径 |
| `title` | 建议 | 本地库里显示的名字。缺省时显示目录名 |
| `file` | 建议 | 入口页相对路径。缺省时按 §6 的顺序回退 |
| `general.properties` | 否 | 用户属性表，见 §3 |

---

## 3. 用户属性（`general.properties`）

属性键名作者自定（`schemecolor`、`newproperty12`、`nightMode` 都行），
值必须按下表给 `type` 与对应形态的 `value`：

| `type` | `value` 形态 | 面板控件 | 备注 |
| --- | --- | --- | --- |
| `color` | 字符串 `"r g b"`，三个 **0..1 浮点**（不是 `#RRGGBB`，不是 0..255） | 取色器 | 面板改完回调里拿到的也是同形态字符串 |
| `slider` | 数字 | 滑杆 | 配合 `min` / `max` / `step` / `precision` |
| `bool` | 布尔 | 开关 | `checkbox` 是它的别名，两者 wire 语义相同（本应用统一归一成 `bool`） |
| `combo` | 字符串 / 数字 / 布尔（保留声明类型） | 下拉 | 必须有非空 `options: [{"label","value"}, …]` |
| `textinput` | 字符串 | 单行文本输入 | |
| `file` | 字符串：相对**壁纸根目录**的路径（下发给页面时会按入口页所在目录补相对前缀，空值不下发） | 文件选择 | 有 `fileType: "image"` 时给图片预览 |
| `directory` | 字符串 | 目录选择 | 存**绝对路径**；配合 `wallpaperRequestRandomFileForProperty` 做轮播 |
| `text` / `group` | —（不参与下发） | 分节标题 | 只在面板显示 `text`，不是可编辑属性 |

元信息字段：

- `order`：面板排序，小的在前。**缺省会排到最后**（校验会给 warning）。
- `text`：面板标签。写 `ui_*` 键时按内置字符串表翻译（目前只有
  `ui_browse_properties_scheme_color` → 「主题颜色」）；其它键会去
  `general.localization` 里查表（`{ "<lang>": { "<key>": "文案" } }`），
  查不到就原样显示。
- `condition`：面板显隐表达式，**只影响 UI**，不影响下发。语法是受限纯表达式：
  `prop.value`、`&&` `||` `!`、`==` `!=` `>` `<` `>=` `<=`、括号、数字/字符串/`true`/`false`。
  例：`clock_enable.value && background_type.value == 1`。
  解析失败按「显示」处理。
- `type` 为 `"text"` 或 `"group"` 的条目是**分节标题**：只在面板里显示 `text`，不可编辑
  （`text` 留空就是一条空白间隔，用来给面板分组，很常见）。
- **完全没有 `type`（或 `type: ""`）的条目会被忽略**：不显示、也不下发。
- 不要自己定义名为 `language` 的属性：应用已内置全局语言（设置 → 通用 → 语言），
  壁纸没声明时自动补一条 `language`；自己声明了则以壁纸声明为准。

`schemecolor` 是 WE 约定的主题色属性名，面板会当成「主题颜色」处理，建议保留。

---

## 4. 页面里能用的宿主接口

页面被放进 sandbox iframe 运行，宿主会在**你的脚本之前**注入一层 WE 兼容 shim，
于是下面这些官方 API 全部可用（都是可选：不用就别写）。

### 4.1 属性回调

```js
window.wallpaperPropertyListener = {
  applyUserProperties(props) {
    // props 形如 { speed: { value: 0.8 }, mode: { value: "aurora" } }
    if (props.speed) params.speed = Number(props.speed.value);
  },
  applyGeneralProperties(props) { /* { fps, … } */ },
  setPaused(paused) { /* 宿主暂停/恢复；停了就别再推进动画 */ },
  userDirectoryFilesAddedOrChanged(prop, files) { /* files: string[] */ },
  userDirectoryFilesRemoved(prop, files) { /* files: string[] */ },
};
```

- **初值靠`project.json` 的默认值自己兜底**：宿主回调是异步来的，
  在它到达前先用 `value` 默认值画第一帧，否则会先闪一帧错误颜色。
- 回调里的值形态与 §3 表格一致（`color` 是 `"r g b"` 字符串）。
- 属性名拼错不会抛错，只是「调了没反应」——**名字逐字对齐 `project.json`**。

### 4.2 系统音频频谱

```js
window.wallpaperRegisterAudioListener((data) => {
  // data: Float32Array(128) = 左 64 段 + 右 64 段，每段 0..1
  // 没有音频采集时全为 0（不是 null），据此可直接画频谱条
});
```

段数不足会补零、超出会截断；16/32 段的降采样由宿主派生。

### 4.3 目录属性随机文件

```js
window.wallpaperRequestRandomFileForProperty("photos", (prop, file) => {
  if (!file) return;            // 目录为空时 file 是空串，必须守卫
  img.src = file;
});
```

### 4.4 系统媒体（Now Playing）

`wallpaperRegisterMediaPropertiesListener` / `…ThumbnailListener` /
`…PlaybackListener` / `…TimelineListener` / `…StatusListener`，
播放状态枚举在 `window.wallpaperMediaIntegration`。做「正在播放」类壁纸时用。

---

## 5. 硬性约束与常见坑

- `project.json` 的 `version`（≥ 1 的整数）与 `tags`（含年龄分级标签）是**必填**的，
  缺了 `project_validate` 直接报 error。
- 版本历史：`project_snapshot` 把当前工程冻结到 `.version/<版本号>/` 并把工作副本的
  `version` 推进一版（顺带清掉过期的 `scene.pkg`）；`project_versions` 看历史，
  `project_rollback` 退回某一版。网页壁纸不用打包，冻结/回退只影响源文件。
- **不要依赖网络**：壁纸是离线跑的，`fetch` 外网会一直挂着。素材放工程里。
- **不要依赖 `localStorage` / `sessionStorage` / cookie**：壁纸页会被反复重载，
  状态存不住，还可能抛异常中断整个脚本。需要持久化的只有 `project.json` 属性。
- **`file:///` 前缀会被 shim 改写**成同源相对路径（`http://127.0.0.1:<port>/media/…`）；
  空值会变成空串。写素材地址直接写工程内相对路径（`assets/a.png`）最稳。
- **页面会铺满壁纸窗口**，不同显示器分辨率/缩放下尺寸都会变：
  `body { margin: 0; overflow: hidden }`，并在 `resize` 里重算画布尺寸。
- **`devicePixelRatio` 可能 > 1**，画布 backing store 记得按 dpr 放大（建议封顶 2），
  否则高分屏上会很糊。
- 脚本必须是**同源可读**的普通脚本；跨域入口会退回裸 iframe（拿不到 shim）。
- **别在脚本里留模板占位内容**：`project_validate` 只看结构，
  画面「有没有真的动」只能靠截图自检。

---

## 6. 入口解析顺序

安装/播放时按这个顺序找入口页：

1. `project.json` 的 `file`（存在且是文件）
2. `web/index.html`
3. `index.html`
4. 工程内第一个 `*.html`（按字典序）

建议始终显式写 `"file": "index.html"`，别依赖回退。

---

## 7. 最小可跑骨架

```html
<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <title>web wallpaper</title>
  <link rel="stylesheet" href="style.css" />
</head>
<body>
  <canvas id="stage"></canvas>
  <script src="main.js"></script>
</body>
</html>
```

```css
html, body { margin: 0; height: 100%; overflow: hidden; background: #05070c; }
#stage { display: block; width: 100vw; height: 100vh; }
```

```js
(() => {
  const canvas = document.getElementById("stage");
  const ctx = canvas.getContext("2d");
  const params = { schemecolor: "0.28 0.62 1", speed: 0.5 };

  window.wallpaperPropertyListener = {
    applyUserProperties(props) {
      if (props.schemecolor) params.schemecolor = props.schemecolor.value;
      if (props.speed) params.speed = Number(props.speed.value);
    },
  };

  function fit() {
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.round(innerWidth * dpr);
    canvas.height = Math.round(innerHeight * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }
  addEventListener("resize", fit);
  fit();

  function frame(t) { /* 画你的画面 */ requestAnimationFrame(frame); }
  requestAnimationFrame(frame);
})();
```

---

## 8. 闭环自检

1. 写文件 → `project_validate`（`ok: true` 才继续）。
2. `project_snapshot` 存一版（可选）：历史版本进 `.version/<版本号>/`，
   看历史 `project_versions`，退回 `project_rollback`。
3. `project_install` → `wallpaper_apply` → `wallpaper_screenshot`。
4. **看截图**：黑屏 / 静止 / 布局错位都要回去改，改完 `project_update` 再截图，
   循环到画面符合预期。同一张壁纸再拍会复用已挂载实例（返回里 `applied: false`），连拍很快；
   首次应用若超时，看错误里附带的渲染器诊断（常见是 WebGL2 / 资源还在加载）。
5. 要验证属性热更新：`wallpaper_screenshot` 前后用属性面板或
   `item_props_set` 改一个属性值，确认画面确实变了。

---

## 9. 已知限制

- 不做 3D / WebGL2 之外的软件回退（宿主环境没有 WebGL2 就会失败）。
- 宿主只注入属性、音频、媒体、指针、滚轮五类通道；
  `window.wallpaperPluginListener` 等更冷门的接口没有实现。
- 指针事件由宿主注入：桌面壁纸窗口收不到真实鼠标事件时，
  CSS `:hover` 点不亮（浏览器 hit-test 决定的），
  交互尽量用注入的 `mousemove` / `click` / `pointer*` 事件。
