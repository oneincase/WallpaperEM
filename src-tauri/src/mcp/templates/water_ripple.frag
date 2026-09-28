// 水面：同一张星空贴图上下翻转当倒影，叠程序化波纹 + 以指针为心的同心涟漪。
varying vec2 v_TexCoord;

uniform float g_Time;
uniform vec2 g_PointerPosition;
uniform sampler2D g_Texture0;

void main() {
	vec2 uv = v_TexCoord;
	// 翻转 Y：同一张星云图倒过来就是「星空倒映在水面」
	vec2 wuv = vec2(uv.x, 1.0 - uv.y);
	float t = g_Time;

	// 三层交叉正弦：大浪 + 中波 + 细纹
	float wave = sin(wuv.x * 21.0 + t * 1.05) * 0.011
	           + sin(wuv.x * 9.0 - t * 0.65 + wuv.y * 5.0) * 0.017
	           + sin(wuv.y * 37.0 + t * 1.7) * 0.005;

	// 指针涟漪：距离指针越近越明显
	vec2 p = vec2(g_PointerPosition.x, 1.0 - g_PointerPosition.y);
	float pd = length((wuv - p) * vec2(1.7, 1.0));
	float ring = sin(pd * 78.0 - t * 5.5) * exp(-pd * 5.5) * 0.028;

	vec2 off = vec2(wave + ring, wave * 0.55 + ring * 0.7);
	vec3 col = texSample2D(g_Texture0, wuv + off).rgb;

	// 越靠下越暗（深水）
	float depth = mix(1.0, 0.42, wuv.y);
	col *= depth;
	// 上下都要**软入软出**：uv.y=0 是图层上缘（水天交界），v=1 是屏幕底。
	// （曾经误用翻转后的 wuv.y 做柔化 → 软化落在屏幕底部，上缘留了一条硬边，
	//   实测与上方辉光的交界处跳变 108，看起来就是"地平线上一道刀切"。）
	float edge = smoothstep(0.0, 0.16, uv.y) * smoothstep(1.0, 0.90, uv.y);
	gl_FragColor = vec4(col * edge, edge);
}
