// 胶片颗粒 + 暗角 + 轻微提对比（比 post_bloom 便宜：不采样邻域，只一趟）
// 挂法：图层用 models/util/fullscreenlayer.json（内容 = 当前已渲染画面），g_Texture0 即画面；
// 强度靠**图层 alpha**（与下层混合），绑个属性就能当滑条。
varying vec2 v_TexCoord;

uniform float g_Time;
uniform sampler2D g_Texture0;

float hash(vec2 p) {
	return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}

void main() {
	vec3 c = texSample2D(g_Texture0, v_TexCoord).rgb;
	vec2 d = v_TexCoord - 0.5;

	// 暗角
	float vig = smoothstep(1.25, 0.35, length(d) * 1.85);
	c *= mix(0.75, 1.0, vig);

	// 颗粒（时间驱动；grain 太强会有"噪点感"，0.03~0.06 比较像胶片）
	float n = hash(v_TexCoord * vec2(1920.0, 1080.0) + fract(g_Time) * 137.0) - 0.5;
	c += n * 0.05;

	// 轻微提对比
	c = (c - 0.5) * 1.06 + 0.5;
	gl_FragColor = vec4(clamp(c, 0.0, 1.0), 1.0);
}
