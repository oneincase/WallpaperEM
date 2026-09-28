# 场景壁纸工程规范（MCP / `type: "scene"`）

面向「用 MCP 工具做一张 WE 原生场景壁纸」的 agent。产出是一个真正的
`scene.pkg`（由 `scene_pack` 打包），由既有渲染库 `webwallgl` 播放。
**动手前先读完本文**；`project_create(type="scene")` 给的是可直接跑通的骨架，
在它上面改，不要另起炉灶。

相关资源：`wallpaperem://templates/scene-basic`（模板文件全文）、
prompt `create_scene_wallpaper`（完整闭环流程）。

---

## 0. 动手前先拿"地图"（不用翻渲染库源码）

| 资源 / 工具 | 用途 |
| --- | --- |
| `wallpaperem://reference/pitfalls` | **症状 → 原因 → 改法**清单（14 条，全部实测复现过：angles 动画挂死、效果缺 `.vert` 静默跳过、solidlayer 硬边、粒子数太少……） |
| `wallpaperem://reference/effects` | 效果链三段式契约、15 个可用 `g_*` uniform（时间/指针/音频频谱/颜色）、顶点与片段模板、四类配方 |
| `wallpaperem://reference/particles` | 渲染库内置粒子贴图的**关键字表**（star/ember/smoke/meteor…按名字程序化生成，不用带位图）、已实现的组件白名单与未实现名单、四套完整预设 |
| `particle_recipe` 工具 | 直接返回**可落盘**的预设 + 材质两个文件（星尘/余烬/星云絮/流星） |
| `effect_scaffold` 工具 | 直接返回自写 shader 的**四件套**（effects json + 材质 + `.frag` + `.vert`）：水面波纹 / 指针光晕 / 音频条 / 七段时钟 / **全屏后期泛光** / **胶片颗粒** |
| `layer_selfcheck` 工具 | **判定某层到底画出来没有**：量该层矩形的均值/方差（白块 = 高均值低方差）+ 交叉核对渲染器诊断（会把「跳过效果（pass 编译失败）」原文带回来）→ verdict + 改法 |
| `pitfall_search` 工具 | 按关键词查坑（比通读 pitfalls 省 token） |
| `wallpaper_diff` 工具 | **判定某个字段/效果到底有没有改变画面**：三帧法（A/A2 同属性得"动画噪声底"，B 换属性）→ 逐像素差 + 16×9 分块同号分析。**动态壁纸不适合当 A/B 试验台**，细微变化请用 1~2 层静态探针工程 |
| `wallpaperem://reference/capabilities` | **能力矩阵（实测）**：文字对象 / 内置泛光 `general.bloom` / composelayer 分组 / parent / 变换关键帧动画 / 属性绑定 vector 字段 / combos 变体 / camera.zoom / 全屏后期 / 内置粒子贴图 —— 每条含 JSON 形状、生效判据、实测数值与坑 |

**全屏后期**（泛光/暗角/颗粒/色散）：图层用 `models/util/fullscreenlayer.json`（内容 = 当前已渲染画面，`g_Texture0` 即画面），放在 `objects` **最后**，**强度就是图层 alpha**（绑 slider 属性即可）。实测 alpha 0→0.62：边缘/中心亮度比 0.21→0.12（暗角）、高光像素 8.32%→8.48%（泛光）。

`project_validate` 也会主动拦两个最痛的坑：`angles` 带关键帧动画（渲染库会让首帧永不完成，见下）、效果 shader 缺 `.vert`（会被静默跳过）。

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
└── （版本历史由创作者自行维护，MCP 服务不再代管 .version/）
```

版本管理不在 MCP 服务里：创作者用 git 或自己的方式维护历史；`.version/` 这类
隐藏目录不进 `scene.pkg`、不进本地库，也不该用 `project_write_file` 去写。

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
  模板里的 `glow` 已经绑在 Aurora Glow 图层的 `alpha` 上，照它写即可；
  `project_validate` 会把「声明了却没人绑」的属性点名（拖了滑块没反应就是因为这个）。
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
  名字**逐字对应**；`scene_pack` 会把它转成同名 `.tex` **打进包**（源图片不进包，
  工程目录里也不会多出 `.tex`）。
- 想手写 `.tex` 也可以，但**不能和同名源图并存**：`scene_pack` 会用 `.tex`、把源图
  丢掉，画面于是和你改的源图不一致。校验器会把「同名 `.tex` + 源图」判为 **error**
  （`贴图同名冲突`），`project_write_file` 写源图时会顺手删掉同名 `.tex`。
- **不要用 WebP**：渲染库会把 WebP 当视频纹理，`scene_pack` 直接报错拒绝。
- 单文件（base64 写入时）上限 **40 MB**；base64 会膨胀 4/3，所以原始图片建议 < 30 MB，
  更大就直接写进工程目录再打包。

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

支持的名字（**与渲染库的 `switch (name)` 分支逐个对齐**，名单外的会被静默忽略；
`project_validate` 会对不认识的名字逐个 warning，`wallpaperem://reference/scene-support`
是同一份清单的机器可读版）：

- **emitter**：**只特判 `boxrandom`**（盒形随机，支持
  `directions`/`sign`/`distancemin|max`/`instantaneous`）；其余任何名字都按**球壳**发射
  （WE 里那个名字是 `sphererandom`，库代码里没有它的字面量 —— 所以校验器不对发射器
  名字报 warning，只提醒别忘了写 `name`）。`rate` 与 `instantaneous` 都缺省时按
  「常驻池」处理（灰尘/星空这类）。
- **initializer**：`lifetimerandom`、`sizerandom`、`colorrandom`、`alpharandom`、
  `velocityrandom`、`rotationrandom`、`angularvelocityrandom`、
  `mapsequencearoundcontrolpoint`、`mapsequencebetweencontrolpoints`
  （前几项都支持 `exponent` 偏置）。
- **operator**：`movement`、`angularmovement`、`alphafade`、`alphachange`、
  `sizechange`、`colorchange`、`turbulence`、`oscillatealpha`、`oscillatesize`、
  `oscillateposition`、`controlpointattract`、`vortex`、`vortex_v2`、`remapvalue`、
  `capvelocity`、`positionoffsetrandom`、`collisionquad`、`collisionplane`、
  `reducemovementnearcontrolpoint`、`maintaindistancebetweencontrolpoints`。
- **renderer**：`sprite`（缺省，名字不认识也按它画）、`spritetrail`、`ropetrail`
  （`rope` 按 `sprite` 处理）。

注意：

- `material` 指向的材质 `passes[0].shader` **必须是 `genericparticle`**。
- 图层上的 `origin` 是发射器位置、`scale` 缩放整个系统、`angles.z` 旋转发射方向。
  **`scale` 同时缩放发射范围**——雾/雪就靠放大 scale 铺满画面。
- `instanceoverride`（写在 `objects[]` 上）可以覆盖粒子参数，
  值里带 `{animation, value}` 的键会按关键帧驱动（如 `alpha` 的呼吸/消散）。

---

## 9. `project_validate` 会检查什么

打包/安装前先过一遍校验（返回 `errors`/`warnings` 列表，不抛异常）。
**`errors` 非空时 `scene_pack` / `project_install` 会直接拒绝**，所以这一步就是
你自查的全部依据 —— 这里的每一条都对应一个「不查就会静默画错」的坑：

- `project.json` 存在且是合法 JSON；`type` 在白名单里；`file` 指向的文件真实存在；
- `version` 是 ≥ 1 的整数；`tags` 里**恰好有一个**年龄分级标签（`Everyone`/`Questionable`/`Mature`）；
- `general.properties` 每个属性：`type` 在白名单里、`value` 与 `type` 匹配、
  `combo` 有非空 `options` 且每项有 `value`；
- `scene.json` 是合法 JSON；`general.orthogonalprojection` 有正数宽高（否则 warning）；
- `objects[]` 每个图层：`id` 不重复、`origin`/`scale`/`angles` 是三元组、
  `size` 是二元组、`visible` 是布尔、`image`/`particle` 引用可解析；
- **引用链**：`image` → `models/x.json` → `material` → `materials/y.json` → `passes[].textures`
  → `materials/<name>.tex` 或 `.png` 存在（缺贴图 = error；同名 `.tex` 与源图并存 = error）；
- **用户属性绑定**：任何字段写成 `{"value":…,"user":"<属性名>"}` 时，该属性必须在
  `project.json` 的 `general.properties` 里存在（对不上 = error）；声明了却没人绑的
  属性会被点名（warning）；
- `parent` 必须指向存在的图层 id、不能指向自己、不能成环（都是 error）；
- `effects[].file` 必须存在；效果里用到非内置 shader 时工程要带 `shaders/<名字>.frag/.vert`
  （缺源码 = warning，那个 pass 会被渲染器跳过）；
- **粒子预设**：文件要能解析、`material` 要指向第一个 pass 是 `genericparticle` 的材质
  （否则 error）、`renderer` 至少一项；`emitter`/`initializer`/`operator`/`renderer` 里
  不受支持的名字会被逐个点名（warning）+ 附上支持清单；
- **关键帧**：`animation.c0` 必须存在且非空、每个关键帧要有数字 `frame` 与 `value`、
  `options.fps > 0`、`options.length ≥ 1`，`mode` 只认 `loop`/`mirror`/`single`；
- 数值与几何合理性（warning）：`alpha`/`brightness` 越界、`scale`/`size` 有 0、
  `origin` 落在设计分辨率之外、`colorBlendMode` 不在文档列出的取值里；
- 还没 `scene.pkg` 时给 warning（提示先 `scene_pack`）。

---

## 10. 闭环自检

1. 写 `scene.json` / `models` / `materials` / 贴图（贴图用 base64 写 `.png`）。
2. （可选）版本备份由创作者自行维护（git 提交、拷贝目录均可）。
3. `scene_pack`（自动转 `.tex` + 打 `scene.pkg`）→ 看返回的 `warnings`。
   装进本地库后，界面「类型」里的 **项目** 筛选（MCP 侧传 `tagGroups: [["$project"]]`）就是
   这一族条目 —— 自建工程是 WallpaperEM 独有的类型，与工坊下载的条目区分开。
4. `project_install`（或 `scene_pack(install=true)` 一步到位）→ 看效果有两条路
   （多屏各挂不同壁纸时，`wallpaper_screenshot` 用 `displayId` 点名拍哪一块屏）：
   - **`wallpaper_preview`（推荐）**：把壁纸挂在主窗口里一张**屏外画布**上渲染并抓帧，
     完全不动桌面壁纸会话，改一处看一眼的循环用它最省事（`framesMs` 可多帧）；
   - `wallpaper_apply` → `wallpaper_screenshot`：看「真正挂到桌面上」的样子（会接管桌面，
     截图走渲染器自抓帧，scene / video / gif / image 三个平台都能截）。
   打包前可以跑 `scene_inspect` 做体检：图层树、每张贴图的尺寸/体积/引用者、没人引用的素材、
   属性绑定命中表、粒子组件支持情况、关键帧摘要，外加一份性能提示（贴图过大/粒子过多）。
   注意 `project_update` 现在是**增量同步**：只拷变化的文件、删源里没有的，返回里会告诉你
   `copied/removed/unchanged`。
5. **看截图**：黑屏多半是 `clearcolor` 太暗 / 图层不可见 / `origin.y` 反了 / 贴图没转成功；
   改完**必须重新 `scene_pack` + `project_install`** 再截图（源码不会自动重打包）。
   首次应用要解析 `scene.pkg`，**冷启动几十秒属正常**（工具默认等 60s，别急着传更小的 `timeoutMs`）；
   同一张壁纸紧接着再拍会复用已挂载实例（返回里 `applied: false`），只花几百毫秒 —— 但**改完工程
   重新安装之后再拍一定会重新挂载**（判据是内容时间，不是 itemId），返回里是 `applied: true`；
   想无条件重挂传 `force: true`。超时报错会附上渲染器最近一条诊断：停在哪一步就是卡在哪一步
   （`mount start` = 还在解析，不是加载失败）。
6. 改用户属性验证热更新（`item_props_set` 或属性面板），确认画面跟着变。
7. 想「改一处看一眼」时把改动做小（一次只动 `clearcolor` 或一个图层），截图更容易判断因果。
8. 验收动画/循环/关键帧用 `wallpaper_screenshot(framesMs=[0, 1000, 3000])`：一次调用返回多帧
   （每帧一个 image 块，顺序与请求一致）——单张图看不出时间维度的问题。
9. 结构化输出：`project_validate` / `scene_inspect` / `scene_pack` / `renderer_diag` /
   `wallpaper_preview` 等工具在 `tools/list` 里声明了 `outputSchema`，调用结果除了文本块
   还带一份 `structuredContent`（同一份 JSON）——客户端可以直接按类型消费，不必二次解析。
   另外工具调用按**资源**串行：`project:<名>`（工程目录）与 `item:<id>`（库内条目）两个
   命名空间，安装/更新两把都拿 —— 并发写文件、打包、预览同一个工程/条目不会读到半成品；
   不同工程/条目照旧完全并行（多把键按字典序获取，不会死锁）。
   离屏预览若用 `keepOpen:true` 保留，实例在 **180s** 没有新的抓帧请求时会自动释放
   （不会把 WebGL 上下文与 pkg 缓存一直占住）。
10. 画面和预期不符时**先读 `renderer_diag`**：效果 pass 被跳过（`[we-scene]` 告警）、
   贴图 404、视频纹理失败、mount 停在哪一步都在那里（最近 300 条，可按 label/条目/时间过滤，
   读之前可 `clear=true` 让下一轮从干净历史开始；主窗口的离屏预览也往这条通道上报，
   所以「预览没出图」同样有迹可循）。

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
- **单次工具调用的请求体上限 64 MB**（`project_write_file` 的单文件上限 40 MB）；
  更大的素材请让 agent 直接写工程目录下的文件，再 `scene_pack`。
- **`wallpaper_screenshot` 目前仅 macOS**（Linux/Windows 会明确报错）；那两类平台上
  只能靠应用内预览或自行截图确认画面。
- **非 macOS 的 `web` 类型截图**：`web` 是独立 iframe，画布抓不到，只能靠平台原生快照
  （只有 macOS 有）。补齐需要 Linux 的 WebKitGTK `webkit_web_view_get_snapshot()` 与
  Windows WebView2 的 `ICoreWebView2CapturePreview`，两者都必须在对应平台编译+实机验证。
- **`wallpaper_preview` 需要主窗口当渲染面**：它把画布挂在主窗口里（隐藏/屏外窗口会被
  WebKit 停帧，实测等不到首帧）。主窗口被关掉或被内存压力看门狗回收时，默认**直接报错**
  并给出替代方案；显式传 `openMainWindow: true` 才允许宿主重新打开主界面（会弹到前台，
  结果里 `openedMainWindow=true`）。
- **`wallpaper_preview` 的保真度**：它用最小挂载（源 + fit + 画质 + 用户属性覆盖），
  不带音频/无缝切换那一套宿主适配 —— 看构图、配色、动画足够，声音相关的效果不做保证。
  `web` 类型抓不到（独立 iframe），请用 `wallpaper_screenshot`（macOS 原生快照）。
- **多帧截图在窗口被完全遮挡时可能拿到相同的几帧**：系统对不可见窗口会降频渲染。
  验收动画时把应用/壁纸切到前台，或优先用 `wallpaper_preview`（主窗口在跑）。
- 超过 `project_write_file` 的 40 MB（base64）时用 **`project_import_asset`** 直接拷宿主
  文件进工程（单文件上限 512 MB）。来源被限制在图片/下载/桌面/文稿/音乐/影片/临时目录与
  工程根，扩展名限 `png/jpg/jpeg/tex/frag/vert/h/json/ogg/wav/mp3/gif/mp4` —— 这是为了让
  MCP 不被当成任意文件读取器，不是随意设的限制。

