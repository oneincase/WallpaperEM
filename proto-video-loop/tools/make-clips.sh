#!/usr/bin/env bash
# 生成 4K 测试片。核心是造出「真无缝」和「硬接缝」两个对照物，
# 否则你没法判断自己在页面上看到的卡顿到底是解码问题还是素材本身不循环。
#
#   无缝片 = 素材正放 + 倒放拼接（回文）。首帧与末帧数学上完全相同，
#            因此 loop 回绕时画面不跳 —— 剩下的任何卡顿都归因于解码/合成。
#   硬接缝 = 普通 testsrc2，末帧和首帧完全不同，用来确认「接缝检测」指标有效。
#
# 用法：bash tools/make-clips.sh

set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p clips
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

command -v ffmpeg >/dev/null || { echo "缺少 ffmpeg：brew install ffmpeg"; exit 1; }

# 硬编优先：macOS 上 VideoToolbox 编 4K 比 libx264 快一个数量级。
# 硬编出来的流同样能被 VideoToolbox 硬解，正好贴近真实壁纸素材。
pick() { if ffmpeg -hide_banner -encoders 2>/dev/null | grep -q " $1 "; then echo "$1"; else echo "$2"; fi; }
H264="$(pick h264_videotoolbox libx264)"
HEVC="$(pick hevc_videotoolbox libx265)"
echo "[clips] H.264 编码器 = $H264"
echo "[clips] HEVC  编码器 = $HEVC"

# 编码封装：硬编走码率，软编走 CRF
enc_h264() { if [ "$H264" = "h264_videotoolbox" ]; then echo "-c:v h264_videotoolbox -b:v 30M -maxrate 40M -bufsize 60M"; else echo "-c:v libx264 -preset veryfast -crf 18"; fi; }
enc_hevc() { if [ "$HEVC" = "hevc_videotoolbox" ]; then echo "-c:v hevc_videotoolbox -b:v 20M -tag:v hvc1"; else echo "-c:v libx265 -preset veryfast -crf 22 -tag:v hvc1"; fi; }

# 1080p 做事，最后一步才放大到 4K。
# 原因：-loop 1 的静止图源 + 逐帧 crop 在 4K 下解码/滤镜开销大，而放大
# 只影响清晰度，不影响循环性与解码路径的对比结论。
#
# 素材选型很关键：不能用 testsrc2 那种帧间剧变的图案。它的相邻帧 PSNR 只有
# 20dB，跟「完全不同的两帧」一个量级 —— verify-clips.sh 就分不出接缝了。
# 改用「镜像无缝纹理 + 匀速平移」：帧间只位移 10.7px，相邻帧 PSNR 能到 40dB+，
# 接缝才会在 PSNR 上明确塌下去。而且匀速直线运动本身最便于肉眼看出顿帧。
# 周期账要算清楚，否则「无缝片」自己就带接缝：
#   a + hflip(a) 的周期是 3840，不是 1920 —— 平移 1920 得到的是翻转版本，画面全变。
#   所以纹理要拼成 a + hflip(a) + a（5760 宽），才能让 1920 宽的窗口平移满
#   3840 的周期而不出界（窗口右缘最远到 3840+1920 = 5760）。
# 周期 P = 3840，速度 640 px/s：
#   无缝片 6s 恰好滚满一个周期 → 末帧与首帧只差一个正常帧位移
#   硬接缝 4s 只滚 2560px 就回绕 → 回绕瞬间跳 2560px ≈ 120 帧的位移
echo "[clips] 1/5 造无缝纹理：图案 + 水平翻转 + 图案 = 5760 宽，周期 3840"
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "testsrc2=size=1920x1080" -frames:v 1 \
  -filter_complex "[0:v]split=3[x1][x2][x3];[x2]hflip[h];[x1][h][x3]hstack=inputs=3[v]" \
  -map "[v]" "$TMP/tile.png"

# 必须用 -framerate 30 固定**输入**帧率。image2 循环输入默认 25fps，
# 而 crop 里的 `n` 是滤镜链看到的帧号 —— 若不在输入端对齐到 30，
# n 会按 25fps 走，再由 -r 30 复制补帧，位移量和帧号双双错位。
echo "[clips] 2/5 无缝片：6s 匀速滚满一个周期 (640 px/s, IPPP 无 B 帧)"
ffmpeg -hide_banner -loglevel error -y -loop 1 -framerate 30 -i "$TMP/tile.png" \
  -vf "crop=1920:1080:x='n*640/30':y=0,format=yuv420p" -t 6 \
  -c:v libx264 -preset veryfast -crf 16 -bf 0 "$TMP/seamless.mp4"

echo "[clips] 3/5 硬接缝片：4s 只滚 2560px 就回绕 (同样 640 px/s)"
ffmpeg -hide_banner -loglevel error -y -loop 1 -framerate 30 -i "$TMP/tile.png" \
  -vf "crop=1920:1080:x='n*640/30':y=0,format=yuv420p" -t 4 \
  -c:v libx264 -preset veryfast -crf 16 -bf 0 "$TMP/hardcut.mp4"

echo "[clips] 4/5 放大到 4K 并编码 (H.264 + HEVC)"
ffmpeg -hide_banner -loglevel error -y -i "$TMP/seamless.mp4" \
  -vf "scale=3840:2160:flags=lanczos,format=yuv420p" $(enc_h264) \
  -movflags +faststart clips/loop-seamless-4k.mp4
ffmpeg -hide_banner -loglevel error -y -i "$TMP/hardcut.mp4" \
  -vf "scale=3840:2160:flags=lanczos,format=yuv420p" $(enc_h264) \
  -movflags +faststart clips/loop-hardcut-4k.mp4
ffmpeg -hide_banner -loglevel error -y -i "$TMP/seamless.mp4" \
  -vf "scale=3840:2160:flags=lanczos,format=yuv420p" $(enc_hevc) \
  -movflags +faststart clips/loop-seamless-4k-hevc.mp4

echo "[clips] 5/5 1080p 基准片（对照：非 4K 时同一路径快多少）"
cp "$TMP/seamless.mp4" clips/loop-seamless-1080p.mp4

echo
ls -lh clips | tail -n +2 | awk '{printf "  %-32s %s\n", $9, $5}'
echo
echo "下一步：bash tools/verify-clips.sh   # 先确认素材本身无缝"
echo "验证硬解（播放中另开一个终端跑）："
echo "  bash tools/probe-decode.sh"
