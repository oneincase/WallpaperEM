// 指针光晕：一枚跟随鼠标的柔光 + 拖尾 + 常驻呼吸涟漪环。
// g_PointerPosition 由渲染器换算到**当前层 UV**（0..1，Y 朝下），详情见 renderer.js。
varying vec2 v_TexCoord;

uniform float g_Time;
uniform vec2 g_PointerPosition;
uniform vec2 g_PointerPositionLast;
uniform vec4 g_PointerState;

void main() {
	vec2 uv = v_TexCoord;
	vec2 p = g_PointerPosition;
	vec2 pl = g_PointerPositionLast;

	// 核心柔光（两层高斯叠加，中心更亮）
	float d = length(uv - p);
	float glow = exp(-d * 26.0) * 1.05 + exp(-d * 7.0) * 0.30;

	// 拖尾：把「上一帧位置 → 当前位置」当胶囊，落在胶囊上的像素发光
	vec2 dir = p - pl;
	float seg = length(dir);
	vec2 q = uv - pl;
	float t = seg > 0.0001 ? clamp(dot(q, dir) / (seg * seg), 0.0, 1.0) : 0.0;
	float trail = exp(-length(q - dir * t) * 55.0) * smoothstep(0.0, 0.012, seg) * 0.7;

	// 呼吸涟漪环：不管有没有鼠标都在扩散，鼠标按下时更亮（g_PointerState.z）
	float pulse = fract(g_Time * 0.55);
	float ring = exp(-abs(d - pulse * 0.30) * 70.0) * (1.0 - pulse) * (0.30 + 0.60 * g_PointerState.z);

	float a = glow + trail + ring;
	vec3 col = mix(vec3(0.30, 0.78, 1.0), vec3(0.92, 0.98, 1.0), clamp(glow * 0.8, 0.0, 1.0));
	gl_FragColor = vec4(col * a, a);
}
