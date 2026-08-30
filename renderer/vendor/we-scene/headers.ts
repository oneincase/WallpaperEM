/**
 * WE（Wallpaper Engine）官方公共 shader 头的重建子集。
 * 场景 pkg 内嵌 shader 缺失 `#include "common.h"` 等公共头时由 ScenePreview 提供。
 * 仅包含 GLSL 内置函数之外的 WE 辅助函数（避免与 GLSL 内建冲突）。
 *
 * [we-scene patch] 这里**不要**定义 hlsl2glsl 已经按名改写的辅助函数：
 *   - `saturate(x)` 会被 rewriteCall 改写成 `clamp(x, 0.0, 1.0)`；
 *   - `lerp` 会被整词替换成 `mix`。
 * 若在此定义它们，声明本身会被一起改写 —— 例如
 *   `float saturate(float x)` → `float clamp(float x, 0.0, 1.0)`（语法错误）、
 *   `float lerp(...) { return mix(...); }` → `float mix(...) { return mix(...); }`（非法递归），
 * 整个 shader 编译失败后效果被静默跳过（表现：人物不眨眼、水波/摇曳全失效）。
 */
export const WE_SHADER_HEADERS: Record<string, string> = {
  'common.h': `// WE common.h（重建子集，供 we-scene 浏览器渲染）
#define M_PI 3.14159265359
#define M_PI_2 1.57079632679
vec2 rotateVec2(vec2 v, float a) {
    float c = cos(a);
    float s = sin(a);
    return vec2(v.x * c - v.y * s, v.x * s + v.y * c);
}
float rand(vec2 n) { return fract(sin(dot(n, vec2(12.9898, 78.233))) * 43758.5453); }
float rand(vec2 n, float m) { return 0.5 + 0.5 * rand(n * m); }
float smoothstep01(float x) { return smoothstep(0.0, 1.0, x); }
vec2 smoothstep01(vec2 x) { return smoothstep(vec2(0.0), vec2(1.0), x); }
vec3 smoothstep01(vec3 x) { return smoothstep(vec3(0.0), vec3(1.0), x); }
vec4 smoothstep01(vec4 x) { return smoothstep(vec4(0.0), vec4(1.0), x); }
`,
};
