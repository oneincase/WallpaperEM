# we-scene（vendored，MIT）

浏览器端 Wallpaper Engine `scene.pkg` 渲染引擎，原仓库：https://github.com/wangkaxds/we-scene

- 本目录为原仓库 `src/` 的只读副本（9 个模块 + LICENSE），未做修改；
- 用法：参考 `src/components/ScenePreview.tsx`；
- 能力：scene.pkg 容器解析、TEX 纹理解码（ARGB8888/RGB565/DXT/LZ4/PNG/JPEG/视频纹理）、HLSL→GLSL 转译、2D 场景渲染（图层/效果链/父子层级/视差）；
- 限制：3D 模型 / 粒子 / 文字 / 声音 / 组件不渲染；shader 依赖 pkg 内嵌（无 WE 安装目录回退）。

## 本项目的补充（非原仓库内容）

1. `headers.ts` —— WE 官方公共头 `common.h` 的重建子集（rotateVec2 等），由 `ScenePreview` 的 shaderResolver 在 pkg 内嵌缺失时提供；`common_perspective.h` / `common_blur.h` / `common_composite.h` 未重建，对应效果会被跳过；
2. `render/renderer.js` —— 打了「效果 pass 编译失败时跳过该 pass」补丁（原版会让整帧渲染抛错），保证主体画面可渲染；
3. `scene/parse.js` —— 补丁：`parseVec3/parseVec2` 支持 `{user, value}` 用户属性包装对象（原版会把受用户属性控制的 scale/position 解析成 0，导致整层不可见、画面全灰）。

