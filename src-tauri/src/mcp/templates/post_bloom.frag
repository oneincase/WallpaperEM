// 全屏后期：亮部泛光 + 暗角 + 胶片颗粒 + 轻微色散，一趟做完。
// 挂法：图层用 models/util/fullscreenlayer.json（内容 = 当前已渲染画面），g_Texture0 即画面。
// 强度靠**图层 alpha**（与下层混合）—— 所以属性绑 alpha 就能当强度滑条，shader 不用改。
varying vec2 v_TexCoord;

uniform float g_Time;
uniform vec2 g_TexelSize;
uniform sampler2D g_Texture0;

float hash(vec2 p) {
	return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}

void main() {
	vec2 uv = v_TexCoord;
	vec2 dir = uv - 0.5;

	// 轻微色散：R/B 沿径向反方向偏移
	float ca = 0.0018;
	vec3 base;
	base.r = texSample2D(g_Texture0, uv + dir * ca).r;
	base.g = texSample2D(g_Texture0, uv).g;
	base.b = texSample2D(g_Texture0, uv - dir * ca).b;

	// 亮部提取 + 5x5 高斯（阈值 0.55，避免整屏发灰）
	vec2 t = g_TexelSize * 2.2;
	vec3 bloom = vec3(0.0);
	float wsum = 0.0;
	for (int i = -2; i <= 2; i++) {
		for (int j = -2; j <= 2; j++) {
			vec2 o = vec2(float(i), float(j)) * t;
			float w = exp(-float(i * i + j * j) * 0.35);
			vec3 sm = texSample2D(g_Texture0, uv + o).rgb;
			bloom += max(sm - 0.55, 0.0) * w;
			wsum += w;
		}
	}
	bloom /= max(wsum, 0.0001);

	vec3 col = base + bloom * 1.45;
	// 暗角
	float vig = smoothstep(1.30, 0.30, length(dir) * 1.9);
	col *= mix(0.68, 1.0, vig);
	// 胶片颗粒（很轻，靠 g_Time 变化）
	float n = hash(uv * vec2(1920.0, 1080.0) + fract(g_Time) * 137.0) - 0.5;
	col += n * 0.025;

	gl_FragColor = vec4(clamp(col, 0.0, 1.0), 1.0);
}
