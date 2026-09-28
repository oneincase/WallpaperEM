// 七段数码时钟：时间从 g_Daytime（当天已过比例）反推，纯 shader 画，不依赖字体/文字层。
varying vec2 v_TexCoord;

uniform float g_Time;
uniform float g_Daytime;

// 第 si 段是否点亮（7 段位掩码：0=a 顶 1=b 右上 2=c 右下 3=d 底 4=e 左下 5=f 左上 6=g 中）
float segOn(int si, float mask) {
	float bit = mod(floor(mask / pow(2.0, float(si))), 2.0);
	return bit;
}

// 单个数字的覆盖率（p ∈ [0,1]²，Y 朝下）
float digitMask(vec2 p, int d) {
	float mask = 0.0;
	if (d == 0) mask = 63.0;
	if (d == 1) mask = 6.0;
	if (d == 2) mask = 91.0;
	if (d == 3) mask = 79.0;
	if (d == 4) mask = 102.0;
	if (d == 5) mask = 109.0;
	if (d == 6) mask = 125.0;
	if (d == 7) mask = 7.0;
	if (d == 8) mask = 127.0;
	if (d == 9) mask = 111.0;

	float hit = 0.0;
	for (int i = 0; i < 7; i++) {
		// 七段的矩形（数字盒坐标，Y 朝下）：a 顶 / b 右上 / c 右下 / d 底 / e 左下 / f 左上 / g 中
		float x0 = 0.18, x1 = 0.82, y0 = 0.02, y1 = 0.16;
		if (i == 1) { x0 = 0.84; x1 = 1.00; y0 = 0.10; y1 = 0.50; }
		else if (i == 2) { x0 = 0.84; x1 = 1.00; y0 = 0.50; y1 = 0.90; }
		else if (i == 3) { x0 = 0.18; x1 = 0.82; y0 = 0.84; y1 = 0.98; }
		else if (i == 4) { x0 = 0.00; x1 = 0.16; y0 = 0.50; y1 = 0.90; }
		else if (i == 5) { x0 = 0.00; x1 = 0.16; y0 = 0.10; y1 = 0.50; }
		else if (i == 6) { x0 = 0.18; x1 = 0.82; y0 = 0.44; y1 = 0.56; }

		float inX = step(x0, p.x) * step(p.x, x1);
		float inY = step(y0, p.y) * step(p.y, y1);
		hit += inX * inY * segOn(i, mask);
	}
	return clamp(hit, 0.0, 1.0);
}

void main() {
	float secs = g_Daytime * 86400.0;
	float hh = floor(secs / 3600.0);
	float mm = floor(mod(secs, 3600.0) / 60.0);
	float ss = floor(mod(secs, 60.0));

	// 8 个插槽：H H : M M : S S（冒号槽窄一些）
	float slotW = 1.0 / 8.0;
	float u = v_TexCoord.x / slotW;
	int slot = int(floor(u));
	float lu = fract(u);
	float v = v_TexCoord.y;

	float on = 0.0;
	// 冒号槽用一个小方点 + 一个下方小方点
	float colon = 0.0;
	if (slot == 2 || slot == 5) {
		float dot1 = step(0.35, lu) * step(lu, 0.65) * step(0.30, v) * step(v, 0.42);
		float dot2 = step(0.35, lu) * step(lu, 0.65) * step(0.58, v) * step(v, 0.70);
		colon = clamp(dot1 + dot2, 0.0, 1.0);
	} else {
		float digit = -1.0;
		if (slot == 0) { digit = floor(hh / 10.0); }
		if (slot == 1) { digit = mod(hh, 10.0); }
		if (slot == 3) { digit = floor(mm / 10.0); }
		if (slot == 4) { digit = mod(mm, 10.0); }
		if (slot == 6) { digit = floor(ss / 10.0); }
		if (slot == 7) { digit = mod(ss, 10.0); }
		if (digit >= 0.0) {
			// 数字盒比槽窄一点，段与段之间留白
			vec2 p = vec2((lu - 0.12) / 0.76, (v - 0.12) / 0.76);
			if (p.x > 0.0 && p.x < 1.0 && p.y > 0.0 && p.y < 1.0) {
				on = digitMask(p, int(digit));
			}
		}
	}

	float lit = clamp(on + colon, 0.0, 1.0);
	// 呼吸感的辉光（g_Time 让它在极慢地明暗，像个活着的表）
	float breath = 0.82 + 0.18 * sin(g_Time * 1.3);
	float a = lit * breath;
	vec3 col = mix(vec3(0.35, 0.85, 1.0), vec3(0.95, 0.99, 1.0), lit);
	gl_FragColor = vec4(col * a, a);
}
