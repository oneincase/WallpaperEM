# we-scene（vendored，MIT）

浏览器端 Wallpaper Engine `scene.pkg` 渲染引擎，原仓库：https://github.com/wangkaxds/we-scene

- 本目录为原仓库 `src/` 的只读副本（9 个模块 + LICENSE），未做修改；
- 用法：参考 `src/components/ScenePreview.tsx`；
- 能力：scene.pkg 容器解析、TEX 纹理解码（ARGB8888/RGB565/DXT/LZ4/PNG/JPEG/视频纹理）、HLSL→GLSL 转译、2D 场景渲染（图层/效果链/父子层级/视差）；粒子系统见下方补充说明；
- 限制：文字 / 组件不渲染；shader 依赖 pkg 内嵌（无 WE 安装目录回退）。

## 本项目的补充（非原仓库内容）

1. `headers.ts` —— WE 官方公共头 `common.h` 的重建子集（rotateVec2 等），由 `ScenePreview` 的 shaderResolver 在 pkg 内嵌缺失时提供；`common_perspective.h` / `common_blur.h` / `common_composite.h` 未重建，对应效果会被跳过；
2. `render/renderer.js` —— 打了「效果 pass 编译失败时跳过该 pass」补丁（原版会让整帧渲染抛错），保证主体画面可渲染；另新增 `setParticleRenderer()` 注入点，让宿主在图层绘制后叠加粒子；
3. `pkg/texture.js` —— 补丁：解析 `.tex` 末尾的 **TEXS 序列帧段**（原版读完 TEXB 图像数据即返回，帧信息被丢弃）。
   实测布局（TEXS0003）：`"TEXS000x\0"` + `u32 frameCount`，TEXS0002/0003 另有 `u32 frameWidth/frameHeight`，
   随后每帧 32 字节 = 8 个 float32 `[imageId, duration秒, x, y, width, 0, 0, height]`。
   雨滴贴图 `particles 256x1280 blank` 即 1280×256、5 帧 256×256 横排，`duration` 0.2s；
4. `scene/parse.js` —— 补丁：用户属性（`{user, value}` 包装）的解析。
   - `parseVec3/parseVec2` 支持包装对象（原版会把受用户属性控制的 scale/position 解析成 0，导致整层不可见、画面全灰）；
   - `parseNum`：`alpha` / `brightness` / `volume` / `pointsize` / `animationlayers.rate` 等数值字段原本按
     `typeof x === 'number'` 判定，包装对象直接落到默认值。本机 78 个场景里 `alpha` 有 188 处绑属性，
     其中 **62 处快照值不是 1**（含 2 处 `alpha: 0`）—— 被强制成 1 后这些半透明层（水汽、光晕、暗角）
     画成了完全不透明，本该隐藏的层也照样显示；
   - `resolveUserProps`：解析前先把整棵对象树的包装解成生效值。默认取场景内快照，仅当宿主在
     `project.json` 上标记了 `userOverridden`（用户显式改过）时才改用属性表的值 —— 快照与属性默认值
     有 372/2598 处不等，无条件改读会让用户没做任何修改、画面就先变样
     （详见仓库根 README 的「壁纸自定义配置」）；
5. `render/particles.js` —— 粒子系统（原版不渲染粒子）。CPU 模拟 + WebGL 实例化 quad；
6. `render/particle-textures.js` —— WE 内置粒子贴图的程序化重建。

## 粒子系统实现说明

覆盖范围按全库 191 个真实粒子系统的使用频次确定：

- 发射器：`sphererandom`、`boxrandom`（含 `directions` / `sign` / `distancemin|max` / `instantaneous`）。
  `rate` 与 `instantaneous` 皆缺省时按「维持池满」的常驻场处理（尘埃/灰烬/星空共 21 例），
  但带 `audioprocessingmode` 的发射器例外——无音频输入时按不发射处理。
- 初始化器：`lifetimerandom`、`sizerandom`、`colorrandom`、`alpharandom`、`velocityrandom`、
  `rotationrandom`、`angularvelocityrandom`、`turbulentvelocityrandom`（均支持 `exponent` 偏置）。
- 算子：`movement`、`angularmovement`、`alphafade`、`alphachange`、`sizechange`、`colorchange`、
  `turbulence`、`oscillatealpha`、`oscillatesize`、`oscillateposition`、`controlpointattract`、
  `vortex`、`remapvalue`。
- 渲染器：`sprite`、`spritetrail`、`ropetrail`（后两者用历史轨迹采样近似；`rope` 按 sprite 处理）。
- 序列帧：优先用 `.tex` 的 **TEXS 段真实帧矩形**；无 TEXS 时才退回按 `sequencemultiplier` 猜 N×N 方格。
  含 `animationmode: randomframe`。
- `children` 子发射器：降级为「与父同图层的独立系统」（不逐粒子跟随），保留本体+光晕+尾迹的分层观感。
- 控制点：`locktopointer` 的控制点跟随鼠标，驱动 `controlpointattract`。
- 音频驱动（`audioprocessingmode`）：本渲染器无音频输入，按「静音时不发射」处理。

### 关键语义（与直觉不同，改动前请留意）

- 图层的 `origin` 是发射器世界位置、`scale` 缩放整个系统、`angles.z` 旋转发射方向；粒子在图层局部空间模拟。
  `scale` **同时缩放发射器范围**：全库 54 个 scale≠1 的天气类层实测，乘 scale 有 17/54 铺满画面，
  不乘只有 2/54 —— 雾/雪正是靠放大 scale 铺满屏幕的。
- `scale` 的 x/y 不等时精灵被**拉伸**（92 个带 scale 的层里 36 个非等比），光柱/雨丝/横向雾带靠此成形；
  故尺寸以较小轴为基准、较大轴作为拉伸比，不能取平均值做等比缩放。
- 精灵 quad 遵循**贴图（单帧）宽高比**，`size` 控制的是**长边**，短边按比例压缩。
  全库 72 个非方形贴图层实测：按「size=长边」只有 4% 的精灵超出屏幕 1.5 倍，按「size=短边」有 18%
  （光轴会算出 36545px 这种荒谬尺寸）。忽略宽高比会把 64×256 的竖光束画成横粗条。
- `instanceoverride` 的 `count` / `rate` / `size` / `speed` / `lifetime` 是**倍率**，不是绝对值。
  其中 `count` 既缩上限**也按同比例缩发射率** —— 稳态存活数 ≈ rate × lifetime，只缩上限的话
  调小 count 只会加快回收、密度不变。全库 91 个带 count override 的层实测：只缩上限有 46% 的层
  稳态超出 maxcount 被硬截断（雨会挤成画面正中一块过密方块），同时缩发射率则降到 13%。
- 发射累加器必须**按发射器各存一份**：低速发射器（光轴 `rate=0.3/s`）每帧只累加 0.005 颗，
  与高速发射器共用累加器时零头会被反复取走，永远发不出粒子（表现为存活数只降不升、光柱像是静止的）。
- 无 `rate` 且无 `instantaneous` 的发射器 = 「维持池满」的常驻场（尘埃/灰烬/星空，全库 21 个）。
  但 `maxcount` 是编辑器里的**预算上限**而非期望值：同一个 `dust_motes_0` 预设在 7 个场景里都是 128，
  只有一个被改成 100000。WE 靠 GPU 预算压制它，故这里对常驻场另设 2048 的封顶。
- `colorn` 是 0–1 归一化色，`color` 是 0–255；混用会把粒子压成近黑。
- `alphafade` 的 `fadeintime` / `fadeouttime` 是**生命周期比例**，不是秒。
- 用 `gl.POINTS` 不可行：`gl_PointSize` 有实现上限（多数 64–255），而 `sizerandom.max` 实测达 2200，
  且点精灵无法旋转（54 个系统用 `rotationrandom`）。

### 贴图为何是程序化生成的

粒子材质引用的贴图大部分**不在 scene.pkg 里** —— 它们随 Wallpaper Engine 安装目录分发，
作者的 pkg 只存自制的工坊素材。全库 33 张被引用的粒子贴图有 24 张属于内置资源。
没有 WE 安装目录可回退，故 `particle-textures.js` 按名字语义程序化生成近似素材
（径向光晕、拉长软斑、噪声云团、光束梯度、花瓣/叶片/星形），未命中的名字走关键词模糊匹配再兜底为通用光晕。

生成时有两条硬约束，破坏其中任一条都会产生明显的渲染瑕疵：

1. **四边 alpha 必须收敛到 0**，否则精灵 quad 会露出硬直边，看起来是一个个矩形方块；
2. **平均 alpha 需压在 0.03–0.12 量级**。这些精灵会被放大到几千像素、十几层 additive 叠加；
   平均 alpha 偏高会直接把画面冲白（曾实测光轴场景 26% 像素过曝、halo_4 使星空场景过曝 16%）。

canvas 绘制的素材（气泡/花瓣/叶片/星）统一经 `conditionTexture()` 做边缘羽化 + 能量归一，
使其与不依赖 canvas 的算术素材表现一致（否则无 `document` 环境与浏览器环境的观感会不一致）。


