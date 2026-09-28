// MCP 资源：**渲染库踩坑知识**的结构化真源。
//
// 为什么要有这个文件：场景壁纸的能力边界散在渲染库源码里（粒子贴图按名字关键字
// 程序化生成、效果链是 effects → materials → shaders 三段式、缺 .vert 会被静默跳过、
// angles 动画会让首帧永不完成……）。agent 每次都要去翻库代码才能动手，既慢又容易踩坑。
// 这里把「这一轮实战里验证过的结论」固化成机器可读的资源，跟渲染库版本一起维护：
//   - wallpaperem://reference/effects    效果链契约 + 可用 uniform + 模板 + 坑
//   - wallpaperem://reference/particles  内置粒子贴图/组件白名单 + 现成配方
//   - wallpaperem://reference/pitfalls   一张「症状 → 原因 → 改法」清单
//
// 约定：这里的**名字**必须与渲染库实现一致（白名单与 workspace::scene_support_json 同源），
// 新增/删除都要连带更新本文件 —— 否则 agent 拿到的是过期地图。

use serde_json::{json, Value};

/// 效果链契约 + uniform + 模板（`wallpaperem://reference/effects`）
pub fn effects_json() -> Value {
    json!({
        "summary": "自写效果着色器要走**效果链**：effects/<名>.json → materials/effects/<名>.json → shaders/effects/<名>.frag **和** .vert。只有 material 那一层写 shader；两边都要在工程里。",
        "fileLayout": {
            "effects/<名>.json": {
                "说明": "图层挂的就是它；passes[] 里写 **material**（不是 shader）",
                "示例": { "passes": [{ "material": "materials/effects/<名>.json", "target": null, "bind": [] }] },
            },
            "materials/effects/<名>.json": {
                "说明": "真正带 shader 的那一层材质",
                "示例": { "passes": [{
                    "shader": "effects/<名>", "blending": "normal", "cullmode": "nocull",
                    "depthtest": "disabled", "depthwrite": "disabled",
                }] },
            },
            "shaders/effects/<名>.frag": "片段着色器（WE 方言 GLSL：varying vec2 v_TexCoord; … gl_FragColor = …;）",
            "shaders/effects/<名>.vert": "顶点着色器，**必须成对提供**。缺它 → 链接失败 → 整趟被静默跳过（图层只剩内置材质的纯色块，诊断通道无记录）",
        },
        "layerUsage": {
            "自带贴图的层": "图层用内置材质正常画（如 genericimage2 + textures:[\"nebula\"]），效果挂在 effects[] 上，shader 里用 g_Texture0 取图层内容（水面扭曲就是这样）",
            "纯发光/覆盖层": "图层用 models/util/solidlayer.json（内置纯色层，工程里不用带文件）+ effects[]；shader 自己输出颜色，忽略 g_Texture0",
            "边界注意": "效果层要在**自己四边**乘一个 margin 淡出（smoothstep(0,v)…），否则图层边界会留一条亮线",
        },
        "transparentPreviewTemplate": {
            "effects": [{ "file": "effects/<名>.json", "name": "<名>", "visible": true }],
        },
        "uniforms": {
            "g_Time": "float，秒。所有程序化动画都用它（波纹/呼吸/闪烁）",
            "g_Daytime": "float 0..1，当天已过比例；秒数 = g_Daytime*86400 → 可做时钟",
            "g_PointerPosition": "vec2，指针位置，**已换算到当前层 UV**（0..1，Y 朝下）",
            "g_PointerPositionLast": "vec2，上一帧指针位置（做拖尾/速度）",
            "g_PointerState": "vec4，.z = 左键是否按下（0/1）——点击特效靠它",
            "g_AudioSpectrum16Left": "float[16]，窄频段；Right 为右声道",
            "g_AudioSpectrum32Left": "float[32]，常用",
            "g_AudioSpectrum64Left": "float[64]，最细",
            "g_Color": "vec3，图层 color（属性绑定的颜色就落在这里；genericimage4/效果 pass 都会拿到）",
            "g_Alpha / g_UserAlpha": "float，图层 alpha",
            "g_Brightness": "float，图层 brightness",
            "g_Texture0": "sampler2D，该 pass 的第 0 张贴图（图层内容或效果链上一趟的输出）",
            "g_ModelViewProjectionMatrix": "mat4，顶点着色器里用 mul(vec4(a_Position,1.0), g_ModelViewProjectionMatrix)",
            "g_EffectModelViewProjectionMatrix": "mat4，效果 pass 的像素空间正交矩阵",
            "g_TexelSize": "vec2，1/分辨率",
        },
        "vertexTemplate": "uniform mat4 g_ModelViewProjectionMatrix;\n\nattribute vec3 a_Position;\nattribute vec2 a_TexCoord;\n\nvarying vec2 v_TexCoord;\n\nvoid main() {\n\tgl_Position = mul(vec4(a_Position, 1.0), g_ModelViewProjectionMatrix);\n\tv_TexCoord.xy = a_TexCoord;\n}\n",
        "fragmentTemplate": "varying vec2 v_TexCoord;\n\nuniform float g_Time;\nuniform sampler2D g_Texture0;\n\nvoid main() {\n\tvec4 c = texSample2D(g_Texture0, v_TexCoord);\n\tfloat a = c.a * (0.6 + 0.4 * sin(g_Time * 2.0));\n\tgl_FragColor = vec4(c.rgb, a);\n}\n",
        "postProcess": {
            "说明": "全屏后期（泛光/暗角/颗粒/色散）走 models/util/fullscreenlayer.json：它的内容就是**当前已渲染画面**，shader 里 g_Texture0 即画面。",
            "强度": "图层 alpha 就是混合强度（与下层未处理画面混合）→ 绑一个 slider 属性即得强度滑条，shader 不用改",
            "顺序": "必须放在 scene.json objects 的**最后**（它读的是它下面的画面）",
            "实测": "postfx 层 alpha 0 → 0.62 时：边缘/中心亮度比 0.21 → 0.12（暗角），>200 高光像素 8.32% → 8.48%（泛光）",
            "验证方法": "layer_selfcheck 会量该层矩形并核对诊断；判「后期是不是生效」也可以拿属性做 A/B（item_props_set 改强度 → wallpaper_screenshot 两次对比）",
        },
        "recipes": {
            "post_bloom": "全屏后期一趟做完：亮部阈值 0.55 提取 + 5x5 高斯泛光 + 暗角 + 胶片颗粒 + 轻微径向色散（g_TexelSize 取偏移步长）",
            "film_grain": "廉价的胶片质感：暗角 + 时间驱动颗粒 + 轻微提对比（不采样邻域，比泛光便宜）",
            "water_ripple": "三层交叉正弦扰动 UV 采样 g_Texture0 + 以 g_PointerPosition 为心的同心环（sin(dist*k - g_Time*ω) * exp(-dist*d)）",
            "pointer_aura": "v_TexCoord 与 g_PointerPosition 求距离做高斯柔光；再拿 g_PointerPositionLast→当前位置做胶囊形状的拖尾；g_PointerState.z 控制点击涟漪环强度",
            "audio_bars": "把 v_TexCoord.x 分 N 柱，按对数取频段（idx = pow(fi,1.7)*32）从 g_AudioSpectrum32Left/Right[int(idx)] 取幅度；静音时给一点 idle 底噪免得整排看不见",
            "clock": "secs = g_Daytime*86400 → hh/mm/ss；用七段位掩码（0→63,1→6,2→91,3→79,4→102,5→109,6→125,7→7,8→127,9→111）在 shader 里画数字，不依赖字体/文字层（渲染库没有文字层）",
        },
        "pitfalls": [
            "effects/<名>.json 里直写 shader（而非 material）会被当成「无 material 的命令 pass」→ 整条链什么都不画且无任何日志（webwallgl#11）",
            "shader 只有 .frag 没 .vert → 静默跳过（webwallgl#10）",
            "material 指向的文件不在包内 → 该 pass 被静默丢弃（webwallgl#11）",
            "效果层边界不做 margin 淡出 → 图层四边留下亮线（实测交界处跳变可达 178 灰度）",
            "用 solidlayer 做发光带 → 纯色矩形是硬边，画面里就是一条横杠；要软边请用渐变贴图（横竖都高斯衰减到黑）",
            "照片贴图直接当加法层 → 照片本身是矩形硬边，先在素材阶段羽化（乘一个边缘渐隐的遮罩）",
        ],
        "diagnostics": "效果没生效先读 renderer_diag：编译失败/缺 stage 这类信息目前只在渲染库的 console.warn 里，宿主诊断通道看不到（webwallgl#10/#11 建议补 diag）—— 所以「画面没有效果」时优先怀疑上面这几条。",
    })
}

/// 内置粒子贴图 + 组件白名单 + 现成配方（`wallpaperem://reference/particles`）
pub fn particles_json() -> Value {
    json!({
        "summary": "粒子预设写在自己工程的 particles/presets/*.json；**贴图不用带位图** —— 贴图名走渲染库内置的程序化生成（按名字关键字决定形状）。",
        "textureNames": {
            "说明": "名字不需要真实存在，渲染库按关键字程序化生成（并按原生尺寸 ×2 生成，避免放大发糊）；也支持显式内置名",
            "keywords": {
                "star / spark / sparkle / 星": "尖星（spikyStar 256）",
                "flare": "变形光斑（flareAnamorphic 256）",
                "halo / halo_1..6 / halo": "多重高斯光晕（通用圆点/尘埃主力）",
                "fire / flame / ember / ash": "火苗（fireWisp 256）→ 余烬用它",
                "smoke": "絮状烟（fogNoise 512）",
                "fog / cloud / mist / vapor": "雾/云（fogNoise 512）→ 星云絮用它",
                "snow / snowflake / 雪花": "雪花（256）",
                "rain": "雨丝（1024×1024 / 4 帧图集）",
                "meteor / shooting / 流星": "流星尾迹团（trailBlob 128×256）",
                "petal / sakura / rosepetal": "花瓣（256）",
                "leaf / leaves": "叶片（3×3 图集，可用 leaves1..8 选帧）",
                "bubble": "气泡（256，带法线变体 bubble1normal）",
                "ring / wave": "圆环（512）",
                "glyph / rune / magic": "符文（256）",
                "beam / shaft / ray / godray": "光束（128×512）",
                "crescent / moon / sickle": "月牙（256）",
                "lightning": "闪电",
                "debris": "碎屑",
                "drop": "水滴",
                "chromaticdot": "色散点",
            },
            "常见内置全名": [
                "particle/halo", "particle/halo_1", "particle/halo_2", "particle/halo_3",
                "particle/halo_6", "particle/fire/fire1", "particle/fire/fire2",
                "particle/fog/fog1", "particle/fog/fog2", "particle/fog/fog3",
                "particle/smoke/smoke2", "particle/light/flare_0", "particle/light/light_shafts_0",
                "particle/nature/rain1", "particle/nature/rain2", "particle/misc/star_0",
                "particle/bubbles/bubble1", "particle/debris/debris1", "particle/drop",
            ],
        },
        "elementNames": {
            "说明": "只有这些名字会被渲染库执行；名单外的名字**静默忽略**（project_validate 会 warning）",
            "emitter": ["boxrandom", "sphererandom", "（其它名字一律按球壳发射，与 sphererandom 同义）"],
            "initializer": ["lifetimerandom", "sizerandom", "colorrandom", "alpharandom",
                            "velocityrandom", "rotationrandom", "angularvelocityrandom",
                            "mapsequencearoundcontrolpoint", "mapsequencebetweencontrolpoints"],
            "operator": ["movement", "angularmovement", "alphafade", "alphachange", "sizechange",
                         "colorchange", "turbulence", "oscillatealpha", "oscillatesize",
                         "oscillateposition", "controlpointattract", "vortex", "vortex_v2",
                         "remapvalue", "capvelocity", "positionoffsetrandom", "collisionquad",
                         "collisionplane", "reducemovementnearcontrolpoint",
                         "maintaindistancebetweencontrolpoints"],
            "renderer": ["sprite", "spritetrail", "ropetrail", "rope"],
        },
        "notImplemented": {
            "turbulentvelocityrandom": "官方预设里很常见，但**渲染库的 switch 里没有这个分支** → 静默忽略。要扰动请用 operator 的 turbulence（参数 scale / speed / phase）。",
        },
        "idioms": {
            "维持池满（尘埃/星空这类常驻场）": "emitter 既不给 rate 也不给 instantaneous —— 粒子池直接填满 maxcount",
            "持续发射": "emitter 给 rate（每秒发射数）",
            "半球/单向发射": "sphererandom + sign（如 sign: \"0 1 0\" = 只向上；directions 用于压扁成线/盘）",
            "拖尾": "renderer 用 spritetrail（length 约 0.006~0.02）",
            "闪烁": "operator oscillatealpha（scalemin/scalemax + frequencymin/frequencymax）",
            "上飘后减速": "movement 给 drag + gravity（如 gravity: \"0 -14 0\"）",
        },
        "recipes": {
            "star_dust": {
                "用途": "满屏缓慢漂浮的星尘/尘埃，带闪烁",
                "texture": "particle/halo_1",
                "preset": {
                    "maxcount": 170, "starttime": 0.5, "material": "materials/presets/star_dust.json",
                    "animationmode": null, "flags": null, "children": null,
                    "emitter": [{ "id": 1, "name": "boxrandom", "distancemax": "960 540 0" }],
                    "initializer": [
                        { "id": 2, "name": "lifetimerandom", "min": 5, "max": 12 },
                        { "id": 3, "name": "sizerandom", "min": 2.5, "max": 7 },
                        { "id": 4, "name": "velocityrandom", "min": "-9 -6 0", "max": "9 6 0" },
                        { "id": 5, "name": "colorrandom", "min": "150 190 255", "max": "255 255 255" },
                        { "id": 6, "name": "alpharandom", "min": 0.25, "max": 0.8 },
                    ],
                    "operator": [
                        { "id": 7, "name": "movement", "drag": 1.4, "gravity": "0 0 0" },
                        { "id": 8, "name": "alphafade", "fadeintime": 0.25, "fadeouttime": 0.55 },
                        { "id": 9, "name": "oscillatealpha", "frequencymin": 0.6, "frequencymax": 2.4, "scalemin": 0.35, "scalemax": 0.9 },
                        { "id": 10, "name": "oscillateposition", "frequencymin": 0.15, "frequencymax": 0.5, "scalemax": 14 },
                        { "id": 11, "name": "turbulence", "scale": 0.0025, "speedmin": 20, "speedmax": 70, "phasemax": 6 },
                    ],
                    "renderer": [{ "id": 1, "name": "sprite" }],
                    "controlpoint": "8 项 {\"flags\":0,\"id\":0..7,\"offset\":\"0 0 0\"}",
                },
            },
            "embers": {
                "用途": "向上飘的暖色火星/余烬（配火苗贴图 + 半球向上 + 拖尾）",
                "texture": "particle/fire/fire1",
                "preset": {
                    "maxcount": 64, "starttime": 1.2, "material": "materials/presets/embers.json",
                    "emitter": [{ "id": 1, "name": "sphererandom", "distancemax": 26, "distancemin": 0,
                                  "directions": "1 1 0", "sign": "0 1 0", "speedmin": 18, "speedmax": 55, "rate": 26 }],
                    "initializer": [
                        { "id": 2, "name": "lifetimerandom", "min": 2.5, "max": 6 },
                        { "id": 3, "name": "sizerandom", "min": 10, "max": 34 },
                        { "id": 4, "name": "colorrandom", "min": "255 150 60", "max": "255 232 170" },
                        { "id": 5, "name": "velocityrandom", "min": "-8 12 0", "max": "8 46 0" },
                        { "id": 6, "name": "rotationrandom", "min": "0 0 -3.14", "max": "0 0 3.14" },
                    ],
                    "operator": [
                        { "id": 7, "name": "movement", "drag": 0.9, "gravity": "0 -14 0" },
                        { "id": 8, "name": "alphafade", "fadeintime": 0.12, "fadeouttime": 0.7 },
                        { "id": 9, "name": "oscillatealpha", "frequencymin": 4, "frequencymax": 14, "scalemin": 0.35, "scalemax": 1 },
                        { "id": 10, "name": "sizechange", "starttime": 0.55, "startvalue": 1, "endvalue": 0.25 },
                        { "id": 11, "name": "turbulence", "scale": 0.004, "speedmin": 40, "speedmax": 120, "phasemax": 5 },
                    ],
                    "renderer": [{ "id": 1, "name": "spritetrail", "length": 0.006 }],
                },
            },
            "nebula_wisps": {
                "用途": "巨大而极慢的絮状团（给星云加层次）",
                "texture": "particle/fog/fog2",
                "preset": {
                    "maxcount": 26, "starttime": 0.5, "material": "materials/presets/nebula_wisps.json",
                    "emitter": [{ "id": 1, "name": "boxrandom", "distancemax": "820 420 0" }],
                    "initializer": [
                        { "id": 2, "name": "lifetimerandom", "min": 9, "max": 18 },
                        { "id": 3, "name": "sizerandom", "min": 320, "max": 760 },
                        { "id": 4, "name": "colorrandom", "min": "70 120 220", "max": "190 150 255" },
                        { "id": 5, "name": "rotationrandom", "min": "0 0 -0.6", "max": "0 0 0.6" },
                        { "id": 6, "name": "velocityrandom", "min": "-6 -3 0", "max": "6 3 0" },
                    ],
                    "operator": [
                        { "id": 7, "name": "movement", "drag": 0.6, "gravity": "0 0 0" },
                        { "id": 8, "name": "alphafade", "fadeintime": 0.45, "fadeouttime": 0.5 },
                        { "id": 9, "name": "sizechange", "starttime": 0.5, "startvalue": 0.55, "endvalue": 1.25 },
                        { "id": 10, "name": "colorchange", "starttime": 0.5, "startvalue": "1 1 1", "endvalue": "0.5 0.5 0.6" },
                        { "id": 11, "name": "oscillatealpha", "frequencymin": 0.05, "frequencymax": 0.25, "scalemin": 0.5, "scalemax": 1 },
                    ],
                    "renderer": [{ "id": 1, "name": "sprite" }],
                },
            },
            "meteors": {
                "用途": "偶发流星（少量、高速、长尾）",
                "texture": "particle/meteor/meteor_0",
                "preset": {
                    "maxcount": 8, "starttime": 1.0, "material": "materials/presets/meteors.json",
                    "emitter": [{ "id": 1, "name": "sphererandom", "distancemax": 900, "distancemin": 300,
                                  "directions": "1 0.45 0", "sign": "1 1 0", "speedmin": 700, "speedmax": 1150, "rate": 0.8 }],
                    "initializer": [
                        { "id": 2, "name": "lifetimerandom", "min": 1.2, "max": 2.4 },
                        { "id": 3, "name": "sizerandom", "min": 26, "max": 64 },
                        { "id": 4, "name": "colorrandom", "min": "180 210 255", "max": "255 255 255" },
                        { "id": 5, "name": "alpharandom", "min": 0.5, "max": 1 },
                    ],
                    "operator": [
                        { "id": 6, "name": "movement", "drag": 0, "gravity": "0 0 0" },
                        { "id": 7, "name": "alphafade", "fadeintime": 0.08, "fadeouttime": 0.55 },
                        { "id": 8, "name": "sizechange", "starttime": 0.6, "startvalue": 1, "endvalue": 0.3 },
                    ],
                    "renderer": [{ "id": 1, "name": "spritetrail", "length": 0.02 }],
                },
            },
        },
        "materialTemplate": {
            "说明": "粒子材质：shader 固定 genericparticle，textures 写内置贴图名，blending 常用 additive",
            "示例": { "passes": [{ "shader": "genericparticle", "blending": "additive", "cullmode": "nocull",
                                   "depthtest": "disabled", "depthwrite": "disabled",
                                   "textures": ["particle/halo_1"] }] },
        },
        "layerFields": "图层用 particle 字段指向预设：{\"particle\":\"particles/presets/<名>.json\", \"origin\":…, \"size\":…}；预设里的 emitter 坐标是**层内局部**坐标，整体位置靠图层 origin",
        "tips": [
            "粒子数太少是最常见的「观感差」原因：官方预设动辄 40~500（尘埃类 128~200）",
            "贴图名带语义关键字即可拿到对应形状，不必带位图；自带位图反而会被当成工程贴图",
            "size 给大（几百像素）配 alpha 低（0.2~0.4）可做「体积感」的雾/絮",
        ],
    })
}

/// 症状 → 原因 → 改法（`wallpaperem://reference/pitfalls`）
pub fn pitfalls_json() -> Value {
    json!({
        "summary": "踩过的坑清单。每条都是实测复现过的：症状 → 原因 → 改法。画面/挂载异常时先扫这里。",
        "items": [
            {
                "id": "angles-animation-hangs",
                "症状": "壁纸永远挂不上：诊断停在 \"renderer started: N layers\" 之后，没有 ready，也没有任何错误；截图/预览一直等到超时",
                "原因": "图层的 angles 带关键帧动画 —— 渲染库会让首帧永不完成，mount() 永久挂起（webwallgl#9）",
                "改法": "角度变化改在 shader 里用 g_Time 做，或用 alpha/color 这类标量字段动画；origin 的动画**不受影响**，可以正常用",
                "验证": "同工程只去掉 angles 动画 → 立刻 ready",
            },
            {
                "id": "effect-missing-vert",
                "症状": "效果完全不生效，图层变成一块纯色（内置材质的白块）；诊断里只有一条含糊的编译错误",
                "原因": "效果 shader 只写了 shaders/effects/<名>.frag，缺 .vert → 顶点阶段没有 main → 链接失败 → 整趟被跳过（webwallgl#10）。\n**注意**：渲染库会把它报进诊断（scene-mount 有一段 console.warn→diag 桥），但报错文本不带 stage、行号是 -1:-1，长这样：`跳过效果（pass 编译失败）: effects/x shader=effects/x 着色器编译失败: ERROR: -1:-1: '' : Missing main()` —— 容易被误读成\"frag 里没写 main\"",
                "改法": "成对提供 .frag + .vert（顶点着色器照抄资源里的 vertexTemplate）；project_validate 现在也会对缺 .vert 主动 warning",
                "验证": "缺 .vert：该区域红像素 0%、均值 255/方差 0（白块），layer_selfcheck 返回 effect_skipped 并带回上面那条诊断原文；补上 .vert 后 23%",
            },
            {
                "id": "effect-shader-field-wrong",
                "症状": "整条效果链什么都不画（只剩内置材质底色），**诊断里一条记录都没有**",
                "原因": "effects/<名>.json 的 passes[] 里直写了 shader；渲染库在效果文件里只认 material（shader 写在材质里）。直写会被当成「无 material 的命令 pass」静默丢弃，且这条路径没有任何日志（webwallgl#11，已实测复核：与缺 .vert 不同，连 console 都没有）",
                "改法": "三段式：effects/<名>.json(passes[].material) → materials/effects/<名>.json(passes[].shader) → shaders/effects/<名>.{frag,vert}；project_validate 现在会对\"效果文件里直写 shader\"主动 warning",
                "验证": "错格式=白块且诊断无记录；改对后立刻出效果",
            },
            {
                "id": "solid-layer-hard-edge",
                "症状": "画面正中出现一条硬边横杠（颜色随绑定的属性变）",
                "原因": "models/util/solidlayer.json 是**纯色矩形**，天生硬边，1852×170 这样的比例在加法混合下就是一条杠",
                "改法": "要发光带就自己生成渐变贴图（横竖都高斯衰减到黑）+ genericimage4（这个变体才应用 g_Color4 染色）",
                "验证": "剖面相邻行跳变 76/118 → 15",
            },
            {
                "id": "photo-layer-hard-edge",
                "症状": "加法发光的照片贴图在画面里露出矩形边界",
                "原因": "照片本身是硬边矩形",
                "改法": "素材阶段羽化：乘一个边缘渐隐的遮罩（PNG 带 alpha，或加法层直接把边缘乘成黑色）",
                "验证": "羽化后交界跳变从 88 降到 20 以内",
            },
            {
                "id": "effect-layer-edge-line",
                "症状": "效果层边界上有一条亮线（如音频条图层顶边）",
                "原因": "shader 在图层边界处仍输出亮像素（柱高顶满时柱头正好压在边界上）",
                "改法": "留头寸（上限 0.86 而不是 1.0）+ 在 shader 里对自己四边乘 margin 淡出",
                "验证": "该行跳变 178 → 消失",
            },
            {
                "id": "postprocess-layer-basics",
                "症状": "全屏后期（泛光/暗角/胶片颗粒）加上去没反应，或者整屏变白/变黑",
                "原因": "① 图层用了 solidlayer 而不是 models/util/fullscreenlayer.json —— 后期层的内容是\"当前已渲染画面\"，用纯色层就取不到画面（g_Texture0 是空的）；② 后期层放在 objects 中间或最前，读到的是\"它下面还没画完\"的内容；③ 把强度写死在 shader 里，没利用图层 alpha",
                "改法": "全屏后期层三件套：models/util/fullscreenlayer.json + 放在 objects **最后** + 强度绑**图层 alpha**（与下层混合，等于强度滑条）。实测：postfx 层 alpha 0→0.62 时，边缘/中心亮度比 0.21→0.12（暗角），>200 高光像素 8.32%→8.48%（泛光）",
                "验证": "拿属性做 A/B：item_props_set 改强度 → wallpaper_screenshot 两次对比；或用 layer_selfcheck 看该层矩形有没有内容",
            },
            {
                "id": "particle-turbulent-ignored",
                "症状": "粒子不动/没有扰动，参数像是没生效",
                "原因": "turbulentvelocityrandom 是官方预设常用写法，但渲染库没实现这个分支 → 静默忽略",
                "改法": "用 operator 的 turbulence（scale/speedmin/speedmax/phasemax）",
            },
            {
                "id": "particle-too-few",
                "症状": "粒子看着很弱、像没开",
                "原因": "maxcount 太小（模板预设只有 10）",
                "改法": "参考 wallpaperem://reference/particles 的 recipes，尘埃类 128~200、余烬 40~64",
            },
            {
                "id": "autosize-overrides-size",
                "症状": "图层尺寸/位置和 scene.json 里写的不一致",
                "原因": "models/*.json 的 autosize:true 会按**贴图宽高比**重算图层尺寸",
                "改法": "要精确控制就设 autosize:false，或让 size 的宽高比与贴图一致",
            },
            {
                "id": "webp-texture",
                "症状": "打包报错或贴图被当视频",
                "原因": "FIF.WEBP === 35 === FIF.MP4，渲染库把 WebP 当视频纹理",
                "改法": "贴图用 PNG/JPEG",
            },
            {
                "id": "tex-and-source-conflict",
                "症状": "改源图没生效 / 打包报「同名冲突」",
                "原因": "materials/ 下同名 .tex 与源图并存时只有一份能进包",
                "改法": "只留源图（scene_pack 会转 .tex）；write_project_file 写源图时会自动删同名 .tex",
            },
            {
                "id": "big-asset-base64",
                "症状": "project_write_file 报文件过大",
                "原因": "base64 写入上限 40MB（还会膨胀 4/3）",
                "改法": "大素材直接下载到临时/下载目录，再用 project_import_asset 拷进工程（走文件系统，不受这个上限约束）",
            },
            {
                "id": "preview-needs-main-window",
                "症状": "wallpaper_preview 报「主窗口不存在」或「预览桥没有响应」",
                "原因": "离屏预览用主窗口当渲染面（屏外/隐藏窗口会被 WebKit 停帧）",
                "改法": "传 openMainWindow:true 让宿主打开主界面，或改用 wallpaper_screenshot（借壁纸窗口渲染）",
            },
            {
                "id": "screenshot-mime",
                "症状": "客户端按声明的 mime 解码图片失败",
                "原因": "（已修）单帧截图走 _image 老字段时丢过 mime，JPEG 被标成 image/png",
                "改法": "现在 mime 一路带到内容块，缺声明时按魔术字节嗅探；如再遇到请报 bug",
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三个资源都要能被序列化，且关键内容不缺（agent 拿到的必须是完整地图）
    #[test]
    fn references_are_serializable_and_complete() {
        let e = effects_json();
        assert!(e["uniforms"]["g_Time"].is_string());
        assert!(e["uniforms"]["g_PointerPosition"].is_string());
        assert!(e["uniforms"]["g_AudioSpectrum32Left"].is_string());
        assert!(e["vertexTemplate"].as_str().unwrap().contains("v_TexCoord"));
        assert!(e["fragmentTemplate"]
            .as_str()
            .unwrap()
            .contains("gl_FragColor"));
        assert!(e["recipes"].as_object().unwrap().len() >= 4);
        assert!(!e["pitfalls"].as_array().unwrap().is_empty());

        let p = particles_json();
        assert!(p["textureNames"]["keywords"].as_object().unwrap().len() >= 10);
        assert!(p["recipes"].as_object().unwrap().len() >= 4);
        assert!(p["notImplemented"]["turbulentvelocityrandom"].is_string());

        let t = pitfalls_json();
        let items = t["items"].as_array().unwrap();
        assert!(items.len() >= 10);
        for it in items {
            for k in ["症状", "原因", "改法"] {
                assert!(it[k].is_string(), "坑清单缺 {k}: {it}");
            }
        }
    }

    /// 效果脚手架声明的 kind 必须都有对应模板（否则 agent 拿到的是空壳）——
    /// 这里用"资源里 recipes 的键"与"脚手架模板文件名"对表，避免两边漂移。
    #[test]
    fn effect_scaffold_kinds_all_have_templates() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mcp/templates");
        let files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        for kind in [
            "water_ripple",
            "pointer_aura",
            "audio_bars",
            "clock",
            "post_bloom",
            "film_grain",
        ] {
            assert!(
                files.iter().any(|f| f == &format!("{kind}.frag")),
                "脚手架 kind {kind} 缺模板文件（应有 {kind}.frag）；现有：{files:?}"
            );
            assert!(
                effects_json()["recipes"].get(kind).is_some(),
                "resources/effects 的 recipes 里缺 {kind}"
            );
        }
    }

    /// 配方里的粒子组件名必须都在白名单里 —— 名字写错会被渲染库静默忽略，
    /// 这是「配方反而害人」的唯一可能，必须由测试挡住。
    #[test]
    fn particle_recipes_only_use_implemented_names() {
        let p = particles_json();
        let mut allowed_emitter: Vec<String> = vec!["boxrandom".into(), "sphererandom".into()];
        let mut allowed_init: Vec<String> = Vec::new();
        let mut allowed_op: Vec<String> = Vec::new();
        let mut allowed_renderer: Vec<String> = Vec::new();
        for (k, dst) in [
            ("emitter", &mut allowed_emitter),
            ("initializer", &mut allowed_init),
            ("operator", &mut allowed_op),
            ("renderer", &mut allowed_renderer),
        ] {
            for n in p["elementNames"][k].as_array().unwrap() {
                let s = n.as_str().unwrap();
                if s.starts_with('(') {
                    continue; // 说明性条目
                }
                dst.push(s.to_string());
            }
        }
        for (kind, recipe) in p["recipes"].as_object().unwrap() {
            let preset = &recipe["preset"];
            let check = |key: &str, allowed: &[String]| {
                for item in preset[key].as_array().cloned().unwrap_or_default() {
                    let n = item["name"].as_str().unwrap_or("");
                    // 渲染库对未知 emitter 名一律按球壳处理，不是静默丢弃
                    assert!(
                        allowed.iter().any(|a| a == n),
                        "配方 {kind} 的 {key}「{n}」不在白名单里（渲染库会静默忽略）"
                    );
                }
            };
            check("emitter", &allowed_emitter);
            check("initializer", &allowed_init);
            check("operator", &allowed_op);
            check("renderer", &allowed_renderer);
            // 未实现的写法不能出现在配方里
            let text = serde_json::to_string(preset).unwrap();
            assert!(
                !text.contains("turbulentvelocityrandom"),
                "{kind} 用了未实现的组件"
            );
        }
    }
}
