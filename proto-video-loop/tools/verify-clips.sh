#!/usr/bin/env bash
# 验证测试片到底是不是真无缝。
#
# 为什么值得单独验：整套测量的前提就是「素材本身不产生接缝」。
# 素材要是自己就顿一帧，页面上量到的接缝就无法归因给解码/合成，实验直接作废。
#
# 判据不是「首帧 == 末帧」——那反而说明有重复帧，循环时会白顿一帧。
# 正确判据是「末帧 与 首帧 的关系，要和 第2帧 与 首帧 的关系一个量级」，
# 也就是回绕处的画面跳变量 ≈ 正常相邻帧的跳变量。
#
# 用 PSNR 度量：
#   真无缝   PSNR(末,首) ≈ PSNR(第2,首)      回绕跳变和正常帧一样
#   有重复帧 PSNR(末,首) = inf               首尾是同一帧 → 每圈顿一帧
#   硬接缝   PSNR(末,首) ≪ PSNR(第2,首)      回绕跳变远大于正常帧
#
# 用法：bash tools/verify-clips.sh

set -uo pipefail
cd "$(dirname "$0")/.."

command -v ffmpeg >/dev/null || { echo "缺少 ffmpeg"; exit 1; }
[ -d clips ] || { echo "clips/ 不存在，先跑 bash tools/make-clips.sh"; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

psnr() { # $1 $2 → 平均 PSNR
  ffmpeg -hide_banner -i "$1" -i "$2" -lavfi psnr -f null - 2>&1 \
    | grep -o 'average:[0-9.a-z]*' | head -1 | cut -d: -f2
}

frame_md5_at() { # $1 文件  $2 帧号（支持负数从末尾数）
  local n="$2"
  if [ "$n" -lt 0 ]; then
    ffmpeg -v error -sseof -0.6 -i "$1" -f framemd5 - 2>/dev/null \
      | grep -v '^#' | awk 'NF{print $6}' | tail -n 1
  else
    ffmpeg -v error -i "$1" -f framemd5 - 2>/dev/null \
      | grep -v '^#' | awk 'NF{print $6}' | sed -n "$((n + 1))p"
  fi
}

printf '%-28s %10s  %10s  %10s  %s\n' '文件' '帧数' 'PSNR(2,1)' 'PSNR(末,1)' '判定'
printf -- '─%.0s' {1..88}; echo

fail=0
for f in clips/*.mp4; do
  [ -e "$f" ] || continue
  name="$(basename "$f")"

  frames=$(ffprobe -v error -select_streams v:0 -count_frames \
             -show_entries stream=nb_read_frames -of csv=p=0 "$f")
  w=$(ffprobe -v error -select_streams v:0 -show_entries stream=width  -of csv=p=0 "$f")
  h=$(ffprobe -v error -select_streams v:0 -show_entries stream=height -of csv=p=0 "$f")

  # 首两帧 + 末帧。ffmpeg 7+ 用 -fps_mode 取代了已删除的 -vsync
  ffmpeg -v error -y -i "$f" -vf "select=eq(n\,0)+eq(n\,1)" -fps_mode passthrough "$TMP/a%02d.png"
  ffmpeg -v error -y -sseof -0.6 -i "$f" -fps_mode passthrough -update 1 "$TMP/last.png"

  p_adj=$(psnr "$TMP/a01.png" "$TMP/a02.png")   # 正常相邻帧
  p_wrap=$(psnr "$TMP/last.png" "$TMP/a01.png") # 回绕处

  md5_first=$(frame_md5_at "$f" 0)
  md5_last=$(frame_md5_at "$f" -1)

  verdict="?"
  case "$name" in
    *hardcut*) verdict="对照：应当报接缝" ;;
    *)
      if [ "$md5_first" = "$md5_last" ]; then
        verdict="✗ 首尾同帧 —— 每圈白顿一帧"
        fail=1
      elif [ "$p_wrap" = "inf" ]; then
        verdict="✗ 回绕无跳变 = 重复帧"
        fail=1
      else
        # 回绕处的 PSNR 只要不明显低于正常相邻帧，就算无缝
        verdict=$(awk -v a="$p_adj" -v w="$p_wrap" 'BEGIN{
          if (a == "" || w == "" || a == "inf" || w == "inf") { print "? 无法判定"; exit }
          d = a - w
          if (d <= 3)       print "✓ 无缝（回绕跳变 ≈ 相邻帧）"
          else if (d <= 8)  print "~ 临界，回绕略跳"
          else              print "✗ 有接缝（回绕跳变远大于相邻帧）"
        }')
        case "$verdict" in ✗*) fail=1 ;; esac
      fi
      ;;
  esac

  printf '%-28s %10s  %8s  %8s  %s\n' "$name" "$frames" "$p_adj" "$p_wrap" "$verdict"
  echo "    ${w}×${h}"
done

echo
cat <<'EOF'
────────────────────────────────────────────────────────────
如果 seamless 片被判成「有重复帧」或「有接缝」，说明 make-clips.sh
的回文裁剪没生效，先修素材再谈页面指标 —— 否则量到的是素材的锅。
────────────────────────────────────────────────────────────
EOF
exit "$fail"
