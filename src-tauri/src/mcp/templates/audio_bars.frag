// 音频条：从内置频谱 uniform 取 32 段，对数分布画成圆头柱，顶部有高光。
varying vec2 v_TexCoord;

uniform float g_Time;
uniform float g_AudioSpectrum32Left[32];
uniform float g_AudioSpectrum32Right[32];

void main() {
	float bars = 36.0;
	float u = v_TexCoord.x;
	float v = 1.0 - v_TexCoord.y;          // 0 = 底
	float slot = floor(u * bars);
	// 对数分布：低频占更宽的位置（听感上更自然）
	float fi = clamp(slot / bars, 0.0, 0.999);
	float band = pow(fi, 1.7);
	int idx = int(clamp(band * 32.0, 0.0, 31.0));
	float amp = 0.5 * (g_AudioSpectrum32Left[idx] + g_AudioSpectrum32Right[idx]);
	// 静音兜底：系统没在放声音时给一条很低的"呼吸"底噪，否则整排柱子等于不存在
	float idle = 0.20 + 0.13 * sin(u * 17.0 + g_Time * 1.5) * sin(fi * 8.0 + g_Time * 0.8);
	// 柱高**留头寸**：顶到 1.0 时柱头高光会正好压在图层上边缘，形成一条突兀的亮线
	float h = clamp(pow(max(amp, idle), 0.72) * 1.20, 0.03, 0.86);

	// 条内：左右留缝 + 上下圆角
	float barU = fract(u * bars);
	float edge = smoothstep(0.0, 0.14, barU) * smoothstep(1.0, 0.86, barU);
	float body = step(v, h) * edge;
	float cap = exp(-abs(v - h) * 55.0) * edge;   // 顶部亮头
	float a = body * 0.45 + cap * 0.95;

	vec3 col = mix(vec3(0.20, 0.62, 1.0), vec3(0.75, 0.97, 1.0), clamp(v / max(h, 0.001), 0.0, 1.0));
	// 效果层在**自己四边**淡出：图层边界上不留任何亮像素，否则和背景交界处就是一道硬边
	float margin = smoothstep(0.0, 0.05, v) * smoothstep(1.0, 0.95, v)
	             * smoothstep(0.0, 0.02, v_TexCoord.x) * smoothstep(1.0, 0.98, v_TexCoord.x);
	a *= margin;
	gl_FragColor = vec4(col * a, a);
}
