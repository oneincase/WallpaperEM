/**
 * WE（Wallpaper Engine）官方公共 shader 头的重建子集。
 * 场景 pkg 内嵌 shader 缺失 `#include "common.h"` 等公共头时由 ScenePreview 提供。
 * 仅包含 GLSL 内置函数之外的 WE 辅助函数（避免与 GLSL 内建冲突）。
 */
export const WE_SHADER_HEADERS: Record<string, string> = {
  'common.h': `// WE common.h（重建子集，供 we-scene 浏览器渲染）
vec2 rotateVec2(vec2 v, float a) {
    float c = cos(a);
    float s = sin(a);
    return vec2(v.x * c - v.y * s, v.x * s + v.y * c);
}
float rand(vec2 n) { return fract(sin(dot(n, vec2(12.9898, 78.233))) * 43758.5453); }
float rand(vec2 n, float m) { return 0.5 + 0.5 * rand(n * m); }
float saturate(float x) { return clamp(x, 0.0, 1.0); }
vec2 saturate(vec2 x) { return clamp(x, 0.0, 1.0); }
vec3 saturate(vec3 x) { return clamp(x, 0.0, 1.0); }
vec4 saturate(vec4 x) { return clamp(x, 0.0, 1.0); }
float lerp(float a, float b, float t) { return mix(a, b, t); }
vec2 lerp(vec2 a, vec2 b, float t) { return mix(a, b, t); }
vec3 lerp(vec3 a, vec3 b, float t) { return mix(a, b, t); }
vec4 lerp(vec4 a, vec4 b, float t) { return mix(a, b, t); }
float smoothstep01(float x) { return smoothstep(0.0, 1.0, x); }
vec2 smoothstep01(vec2 x) { return smoothstep(vec2(0.0), vec2(1.0), x); }
vec3 smoothstep01(vec3 x) { return smoothstep(vec3(0.0), vec3(1.0), x); }
vec4 smoothstep01(vec4 x) { return smoothstep(vec4(0.0), vec4(1.0), x); }
`,
};
