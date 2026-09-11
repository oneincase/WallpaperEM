# 场景壁纸工程规范（MCP / `type: "scene"`）

面向「用 MCP 工具做一张 WE 原生场景壁纸」的 agent。产出是一个真正的
`scene.pkg`（由 `scene_pack` 打包），由既有渲染库 `webwallgl` 播放。
**动手前先读完本文**；`project_create(type="scene")` 给的是可直接跑通的骨架，
在它上面改，不要另起炉灶。

相关资源：`wallpaperem://templates/scene-basic`（模板文件全文）、
prompt `create_scene_wallpaper`（完整闭环流程）。

---

## 1. 工程结构与交付物

工程根：`~/Documents/WallpaperEM/Projects/<工程名>/`

```
<工程名>/
├── project.json                必需：type/title/file/version/tags + 用户属性
├── scene.json                  必需：场景本体（打包进 pkg 的根）
├── models/*.json               图层用的模型（image 指向它）
├── materials/
│   ├── <名字>.json             材质（passes → shader + textures）
│   └── <名字>.png             贴图源；scene_pack 会就地转成 <名字>.tex
├── particles/presets/*.json    粒子预设
├── shaders/*.frag|.vert        只有用非内置 shader 时才需要（见 §6）
├── sounds/、effects/…          按需
└── .version/<版本号>/          版本历史（工具自动维护，别手写）
```

`.version/` 由 `project_snapshot` 写入：每个版本一份完整源文件副本，
配上 `project_versions`（看历史）与 `project_rollback`（退回）。它**不进 `scene.pkg`、
不进本地库**，也不该用 `project_write_file` 去写。

`scene_pack` 把**除 `project.json`、`preview.*`、`scene.pkg`、`*.md`/`*.txt`、
隐藏项之外的所有文件**打进 `scene.pkg`（`materials/**` 下的 PNG/JPEG 会先转 `.tex`）。
`<工程名>/scene.pkg` 就是最终交付物。

---

## 2. `project.json`

```json
{
  "type": "scene",
  "title": "雪山极光",
  "file": "scene.json",
  "version": 1,
  "tags": ["Landscape", "Everyone"],
  "general": { "properties": { "glow": { "order": 1, "text": "辉光强度",
      "type": "slider", "value": 0.55, "min": 0, "max": 1, "step": 0.05, "precision": 2 } } }
}
```

- `type` 必须是 `"scene"`；`file` 写 `"scene.json"`。
- `version`（**必填**）：≥ 1 的整数，当前这一版的版本号。它同时是历史目录
  `.version/<version>/` 的目录名，所以只能是整数，不能写 `"1.0.0"`。
- `tags`（**必填**）：分类标签数组。**必须恰好含一个年龄分级标签**，取值只能是与 Steam
  工坊逐字符一致的 `Everyone`（大众级，默认）/ `Questionable`（指导级）/ `Mature`（成人级）；
  其余分类标签按工坊口径自由加（`Anime`/`Landscape`/…）。
  模板给的是 `["Everyone"]`，按内容实际上调，别默认留在大众级。
- `general.properties` 的字段与取值规范**与网页壁纸完全一致**
  （`color`/`slider`/`bool`(`checkbox`)/`combo`/`textinput`/`file`/`directory`/
  `text`(`group`) 分节标题；`order`/`text`/`condition`/`options`/`min`/`max`/`step`/`precision`），
  详见 `wallpaperem://docs/web-project` §3。`language` 不用自己声明，应用会自动补。

---

## 3. `scene.json`

```json
{
  "general": {
    "orthogonalprojection": { "width": 1920, "height": 1080 },
    "clearcolor": "0.020 0.030 0.060",
    "clearenabled": true,
    "cameraparallax": true,
    "cameraparallaxamount": 0.6,
    "cameraparallaxdelay": 0.05,
    "cameraparallaxmouseinfluence": 0
  },
  "camera": { "center": "0 0 -1", "eye": "0 0 0", "up": "0 1 0" },
  "objects": [ /* 图层，见 §4 */ ]
}
```

- **`general.orthogonalprojection.{width,height}` 是设计分辨率**，场景坐标 / 图层尺寸
  都按它算，最终按显示模式缩放到窗口。**不写会退化成窗口尺寸**（校验会给 warning）。
- `clearcolor` 是 `"r g b"`（0..1），`clearenabled` 控制是否清屏。
- `camera` 影响视差与 3D 类图层；正交场景里给个标准值即可。
- 全部向量/颜色都是**空格分隔字符串**（`"960 540 0"`、`"1 1 1"`），
  不是数组、不是 `{x,y,z}`。数字整体也是字符串也没问题（`parseNum` 兼容）。

---

## 4. 图层（`objects[]`）

```json
{
  "id": 1,
  "name": "Background",
  "image": "models/background.json",
  "origin": "960.000 540.000 0.000",
  "scale": "1.000 1.000 1.000",
  "size": "1920.000 1080.000",
  "angles": "0.000 0.000 0.000",
  "parallaxDepth": "1.000 1.000",
  "visible": true
}
```

| 字段 | 说明 |
| --- | --- |
| `id` | 场景内唯一整数（用于 `parent` / `instanceoverride`） |
| `name` | 图层名，只影响可读性 |
| `image` | **图层内容**：`models/xxx.json`（贴图/纯色层），或 `models/util/*` 内置模型 |
| `particle` | 图层是粒子系统时，指向 `particles/…json` 预设（与 `image` 二选一） |
| `origin` | 图层**盒子中心**，Y 轴向上（见下） |
| `size` | 盒子尺寸（WE 单位，通常 = 正交分辨率或素材宽高） |
| `scale` | 盒子缩放；**会同时缩放 size**（quad = size × scale） |
| `angles` | 欧拉角（弧度）；`z` 是画面内旋转 |
| `parallaxDepth` | `"x y"`，视差强度（见下） |
| `visible` | 布尔（可被用户属性/脚本控制） |
| `alpha` / `brightness` / `color` | 透明度（0..1）/ 亮度（默认 1）/ 染色 `"r g b"` |
| `parent` | 父图层 id：本层 origin/scale/angles 变成**父级局部空间** |
| `colorBlendMode` | 数字，与背景的混合模式（0/缺省 = 正常，2 = 正片叠底，6/9/31 = 加法，7 = 滤色） |
| `effects` | `[{ "file": "effects/xxx.json", "name": "...", "visible": true }]` 效果链 |
| `animationlayers` | 多动画层（用于 MDL 骨骼，v1 基本用不到） |
| `instanceoverride` | 粒子参数覆盖（见 §8） |

### 4.1 坐标约定（最容易踩的点）

- **Y 轴向上、原点在左下角**：`origin.y = 0` 在**底部**，`origin.y = 正交高度` 在顶部。
  即 `"960 540 0"` 才是画面正中（1920×1080 时）。
- `origin` 是**盒子中心**，不是左上角：`left = origin.x - size.x*scale.x/2`。
- 盒子内容按 `size × scale` 拉伸；素材宽高比与 `size` 不一致就会被拉伸
  （想要等比就自己按素材宽高设 `size`）。
- `angles.z` 单位是**弧度**。

### 4.2 视差

- `parallaxDepth` 写在**哪个图层上就影响哪个图层**；写在**容器（composelayer）**上时
  会**下发给直接子层**（子层自己也有 `parallaxDepth` 时以子层为准）。
- 正值 = 前景：随鼠标位移更大、**方向与鼠标相反**；负值 = 远景：位移小且方向与鼠标相同；
  缺省 / 0 = 不视差。
- 全局强度由 `general.cameraparallaxamount`（0..1 量级）与
  `cameraparallaxmouseinfluence` 共同决定，`cameraparallaxdelay` 是跟随迟滞（秒）。

### 4.3 内置模型（`models/util/*`，工程里不用带）

| 路径 | 用途 |
| --- | --- |
| `models/util/solidlayer.json` | **纯色矩形**（无贴图；用 `color` 上色） |
| `models/util/composelayer.json` | **分组容器**：把子层合并绘制，可整组套效果 |
| `models/util/projectlayer.json` / `fullscreenlayer.json` | 全屏后期处理层：内容 = 当前已渲染画面，套效果后合成回去（体积光/水波/泛光就靠它） |

用这些时**不需要**工程里有对应的 model/material 文件，渲染库内置；
`project_validate` 会跳过 `models/util/` 前缀的引用链检查。

---

## 5. 用户属性绑定

任何字段都可以从"字面值"换成**绑定包装**，让用户在属性面板里改：

```json
"alpha": { "value": 0.55, "user": "glow" },
"origin": { "value": "960 540 0", "user": "pos" }
```

- `user` 是 `project.json` 里属性的名字；`value` 是**默认/快照值**。
- **重要**：用户**没改**属性面板时，场景用 `value` 快照（不是 project.json 里的
  `value`）。所以两处默认值请保持一致，否则「用户还没动，画面就和预期不一样」。
- 可绑定的字段不限于数值：`origin`/`scale`/`angles`/`size`/`alpha`/`brightness`/
  `color`/`visible`/`volume`/`zoom`/`maxwidth` 等都能绑。

---

## 6. 模型 / 材质 / 贴图

`models/<名字>.json`：

```json
{ "autosize": true, "material": "materials/<名字>.json" }
```

`materials/<名字>.json`：一份 `passes` 数组。

```json
{ "passes": [ {
  "shader": "genericimage2",
  "textures": ["bg"],
  "blending": "translucent",
  "cullmode": "nocull",
  "depthtest": "disabled",
  "depthwrite": "disabled"
} ] }
```

- **`shader`**：内置（渲染库用 copy pass 直通，**不需要工程带源码**）——
  `genericimage`（+ 数字后缀 `genericimage2`…）、`sprite`、`flat`、`genericparticle`。
  其它名字必须在工程里带 `shaders/<name>.frag`（+ 可选 `.vert`），否则该 pass 会被跳过。
- **`textures`**：名字按下面的规则解析，**顺序即 `g_Texture0/1/…`**。
  - `"bg"` → pkg 里的 `materials/bg.tex`
  - `"util/xxx"` → 内置贴图
  - `"particle/xxx"` → **内置粒子贴图**（程序化重建，不需要素材，任意名字都行；
    名字里带 `star`/`snow`/`rain`/`smoke`/`fire`/`leaf`/`flare` 等会被识别成对应形状）
  - `"_rt_xxx"` → 效果链的渲染目标（只在 `effects` 的 pass 里用）
- **`blending`**：`normal`（不混合/不透明）、`translucent`（Alpha 混合）、
  `additive`（加法）。真实语料里就这三种。
- `cullmode`：`nocull` / `normal`；`depthtest` / `depthwrite`：`disabled` / `enabled`。
  2D 图层照抄模板的 `nocull` + `disabled` 即可。

### 贴图文件怎么放

- 贴图源放 **`materials/<名字>.png`（或 `.jpg`/`.jpeg`）**，名字与 `textures` 里的
  名字**逐字对应**；`scene_pack` 会就地转成同名 `.tex` 并打进包（源图片不进包）。
- 想手写 `.tex` 也可以（同名 `.tex` 优先，不会被覆盖）。
- **不要用 WebP**：渲染库会把 WebP 当视频纹理，`scene_pack` 直接报错拒绝。

---

## 7. 关键帧动画

字段值可以带 `animation` 做关键帧（`c0` 是主通道）：

```json
"alpha": {
  "value": 0.55,
  "animation": {
    "c0": [
      { "frame": 0,   "value": 0.35, "front": { "enabled": true, "x": 0.33, "y": 0 },
        "back": { "enabled": true, "x": -0.33, "y": 0 }, "lockangle": true, "locklength": true },
      { "frame": 300, "value": 0.35 }
    ],
    "relative": false,
    "options": { "fps": 30, "length": 300, "mode": "loop", "name": "辉光呼吸" }
  }
}
```

- 关键帧：`{ "frame": <帧号>, "value": <值> }`，可选 `front`/`back`
  （**贝塞尔手柄**：`front` 是向前一段的出口手柄、`back` 是向后一段的入口手柄；
  `x` 是相对段长的比例、`y` 是值偏移；`enabled: true` 才生效）。
- `options.fps` 默认 30，`options.length` 是**总帧数**（决定循环周期）。
- `options.mode`：`loop`（循环）/ `mirror`（往返）/ `single`（播一次停住，缺省）。
- `relative: true` 时关键帧值是**相对基值 `value` 的偏移**，否则是绝对值。
- v1 只用 `c0` 通道（多通道 `c1`… 是 WE 高级特性，需要配套骨骼，不在支持范围）。

---

## 8. 粒子预设（`particles/presets/*.json`）

```json
{
  "maxcount": 10,
  "starttime": 15,
  "material": "materials/presets/fireflies.json",
  "emitter":   [ { "id": 5, "name": "sphererandom", "origin": "0 0 0",
                   "directions": "1 0.5 0", "distancemin": 32, "distancemax": 512, "rate": 1 } ],
  "initializer": [ { "id": 2, "name": "lifetimerandom", "min": 3, "max": 20 },
                   { "id": 3, "name": "sizerandom", "min": 70, "max": 90 },
                   { "id": 4, "name": "colorrandom", "min": "136 255 80", "max": "174 255 106" } ],
  "operator":  [ { "id": 6, "name": "movement", "drag": 2.5, "gravity": "0 0 0" },
                 { "id": 7, "name": "alphafade", "fadeintime": 0.1, "fadeouttime": 0.9 } ],
  "renderer":  [ { "id": 1, "name": "sprite" } ],
  "controlpoint": [ /* 8 个 {flags,id,offset}，模板里抄 */ ],
  "children": null,
  "animationmode": null,
  "flags": null
}
```

支持的名字（按全库使用频次实现，其余名字会被忽略）：

- **emitter**：`sphererandom`（球形随机）、`boxrandom`（盒形随机，
  支持 `directions`/`sign`/`distancemin|max`/`instantaneous`）。
  `rate` 与 `instantaneous` 都缺省时按「常驻池」处理（灰尘/星空这类）。
- **initializer**：`lifetimerandom`、`sizerandom`、`colorrandom`、`alpharandom`、
  `velocityrandom`、`rotationrandom`、`angularvelocityrandom`、
  `turbulentvelocityrandom`（都支持 `exponent` 偏置）。
- **operator**：`movement`、`angularmovement`、`alphafade`、`alphachange`、
  `sizechange`、`colorchange`、`turbulence`、`oscillatealpha`、`oscillatesize`、
  `oscillateposition`、`controlpointattract`、`vortex`、`remapvalue`。
- **renderer**：`sprite`、`spritetrail`、`ropetrail`（`rope` 按 `sprite` 处理）。

注意：

- `material` 指向的材质 `passes[0].shader` **必须是 `genericparticle`**。
- 图层上的 `origin` 是发射器位置、`scale` 缩放整个系统、`angles.z` 旋转发射方向。
  **`scale` 同时缩放发射范围**——雾/雪就靠放大 scale 铺满画面。
- `instanceoverride`（写在 `objects[]` 上）可以覆盖粒子参数，
  值里带 `{animation, value}` 的键会按关键帧驱动（如 `alpha` 的呼吸/消散）。

---

## 9. `project_validate` 会检查什么

打包/安装前先过一遍校验（返回 `errors`/`warnings` 列表，不抛异常）：

- `project.json` 存在且是合法 JSON；`type` 在白名单里；`file` 指向的文件真实存在；
- `version` 是 ≥ 1 的整数；`tags` 里**恰好有一个**年龄分级标签（`Everyone`/`Questionable`/`Mature`）；
- `general.properties` 每个属性：`type` 在白名单里、`value` 与 `type` 匹配、
  `combo` 有非空 `options` 且每项有 `value`；
- `scene.json` 是合法 JSON；`general.orthogonalprojection` 有正数宽高（否则 warning）；
- `objects[]` 每个图层：`id` 不重复、`origin`/`scale`/`angles` 是三元组、
  `size` 是二元组、`image`/`particle` 引用可解析；
- **引用链**：`image` → `models/x.json` → `material` → `materials/y.json` → `passes[].textures`
  → `materials/<name>.tex` 或 `.png` 存在（缺贴图 = error，还没转 `.tex` = warning）；
- 还没 `scene.pkg` 时给 warning（提示先 `scene_pack`）。

`errors` 非空时 `scene_pack` 会拒绝打包。

---

## 10. 闭环自检

1. 写 `scene.json` / `models` / `materials` / 贴图（贴图用 base64 写 `.png`）。
2. `project_snapshot` 存一版（可选，但改大动作前后建议存）：冻结到 `.version/<版本号>/`，
   工作副本的 `version` 自动 +1。看历史用 `project_versions`，退回用 `project_rollback`。
3. `scene_pack`（自动转 `.tex` + 打 `scene.pkg`）→ 看返回的 `warnings`。
   注意：冻结会让 `project.json` 的版本号变化，从而清掉旧的 `scene.pkg`，打包要放在冻结之后。
4. `project_install` → `wallpaper_apply` → `wallpaper_screenshot`。
5. **看截图**：黑屏多半是 `clearcolor` 太暗 / 图层不可见 / `origin.y` 反了 / 贴图没转成功；
   改完**必须重新 `scene_pack` + `project_install`** 再截图（源码不会自动重打包）。
   首次应用要解析 `scene.pkg`，**冷启动几十秒属正常**（工具默认等 60s，别急着传更小的 `timeoutMs`）；
   同一张壁纸再拍会复用已挂载实例（返回里 `applied: false`），只花几百毫秒。超时报错会附上渲染器
   最近一条诊断：停在哪一步就是卡在哪一步（`mount start` = 还在解析，不是加载失败）。
6. 改用户属性验证热更新（`item_props_set` 或属性面板），确认画面跟着变。

---

## 11. v1 已知限制

- **不支持自定义 HLSL 效果**：只能吃内置 `genericimage*/sprite/flat/genericparticle`
  直通 shader；带自定义 shader 的 `effects/*.json` 会被跳过。
  （更复杂的效果可以自己写 `effect.json` + material + `shaders/*.frag|.vert` 进工程，
  但没有预设，且不保证所有 WE 公共头都可用。）
- **不支持 3D 模型（`.mdl`）与骨骼木偶**：`model`/`puppet` 字段会被忽略。
- **不支持文字对象**（`text` 字段虽然能读，但排版能力有限，v1 不用）。
- **音频可视化**：`audioprocessingmode` 类发射器在无音频输入时不发射；
  想做频谱请用 `projectlayer/fullscreenlayer` + 自定义 shader（属高级用法，v1 不提供）。
- 贴图格式：PNG / JPEG；**WebP 会被拒绝**。
- 打包格式 `PKGV0022`，`.tex` 用 `freeImage` 直接内嵌原图片
  （PNG 原样、JPEG 原样），渲染库按 V3 布局读。
