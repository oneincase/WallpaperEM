#!/usr/bin/env bash
# 判断「到底是不是硬解」。
#
# 这件事 JS 侧查不到 —— `getVideoPlaybackQuality()` 只给丢帧数，不告诉你用了哪条解码路径。
# 只能从进程侧看，两个独立证据：
#
#   1. VTDecoderXPCService —— macOS 的 VideoToolbox 硬解宿主进程。播放期间它存在
#      且 CPU > 0，就说明确实走了硬件解码器。
#   2. WebContent 进程 CPU —— 4K/30fps 软解约 200%~400% CPU；硬解通常在 30% 以下
#      （剩下的开销是纹理上传或合成，不是解码本身）。
#
# 用法：先在浏览器里开始播放，再跑 bash tools/probe-decode.sh

set -uo pipefail
ROUNDS="${1:-3}"
INTERVAL="${2:-2}"

echo "采样 ${ROUNDS} 次，间隔 ${INTERVAL}s —— 请让视频持续播放中"
echo

for i in $(seq 1 "$ROUNDS"); do
  echo "── 第 $i 次 ──────────────────────────────────────"

  # 证据 1：硬解宿主进程
  vt="$(ps -Ao pcpu,comm 2>/dev/null | grep -i '[V]TDecoderXPCService')"
  if [ -n "$vt" ]; then
    echo "  硬解宿主  ✓ 存在"
    echo "$vt" | awk '{printf "            CPU %5s%%  %s\n", $1, $2}'
  else
    echo "  硬解宿主  ✗ 没有 VTDecoderXPCService"
    echo "            → 要么正在软解，要么解码器跑在 GPU 进程内联（macOS 新版本可能如此），"
    echo "              请结合下面的 CPU 数字一起判断"
  fi

  # 证据 2：渲染进程 CPU。WebKit 的视频解码可能在 GPU 进程或 WebContent 进程里。
  echo "  渲染进程 CPU："
  ps -Ao pcpu,pid,comm 2>/dev/null \
    | grep -Ei '[S]afari|[W]ebContent|[W]ebKit\.GPU|[N]etworking' \
    | sort -rn | head -4 \
    | awk '{printf "            %6s%%  pid %-7s %s\n", $1, $2, $3}'

  echo
  [ "$i" -lt "$ROUNDS" ] && sleep "$INTERVAL"
done

cat <<'EOF'
────────────────────────────────────────────────
判读：
  VTDecoderXPCService 有 CPU + 渲染进程 < 30%   → 硬解 + 直通合成（direct 路径的理想态）
  VTDecoderXPCService 有 CPU + 渲染进程 100%+   → 硬解，但像素被拉回 JS/WebGL（本原型的 webgl 路径）
  没有硬解宿主 + 渲染进程 200%+                 → 软解，回去检查编码格式（需 H.264/HEVC）
────────────────────────────────────────────────
EOF
