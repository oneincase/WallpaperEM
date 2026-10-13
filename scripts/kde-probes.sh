#!/usr/bin/env bash
# KDE 机制探针：在**活的 KDE 会话里**逐条实测 docs/kde-adaptation-plan.md §7 列出的「必须在真机上
# 才能确认的开放问题」。输出形如 `PROBE <名字> = <值>`，便于人读，也便于直接贴回方案文档。
#
# 与 scripts/linux-kde-smoke.sh 的分工：
#   - 本脚本：不需要 WallpaperEM 二进制，只测「KDE 侧到底支不支持我们要用的机制」；
#   - linux-kde-smoke.sh：需要应用二进制，测「我们的窗口在 KDE 下真的落对位置」。
#
# 覆盖的开放问题：
#   Q1 evaluateScript 能否拿到 desktop containment 的 wallpaper 对象（机制①，最关键）
#   Q3 prefs 里桌面 containment 的 plugin/formfactor 键长什么样（机制②的可实施性）
#   Q4 KWin 规则：Layer=Desktop 能否把普通窗口强制放进 DesktopLayer
#   Q6 kwin_wayland --virtual 是否可用
#   Q7 Tauri/GTK 的 WM_CLASS 形态（需要应用在跑时才测得到；这里测 KWin 脚本 API 能读到什么）
#
# 用法（在 KDE 图形会话内，或 SSH 进来但显式给 DISPLAY/DBUS）：
#   bash scripts/kde-probes.sh
#   WAYLAND_DISPLAY=wayland-0 bash scripts/kde-probes.sh    # 在 Wayland 会话内
#
# 退出码：0 探针跑完（不代表都"支持"）；2 前置缺失

set -uo pipefail

FAILED=0
pass() { printf '  \033[32m[PASS]\033[0m %s\n' "$*"; }
fail() { printf '  \033[31m[FAIL]\033[0m %s\n' "$*"; FAILED=$((FAILED+1)); }
warn() { printf '  \033[33m[WARN]\033[0m %s\n' "$*"; }
info() { printf '  [info] %s\n' "$*"; }
probe() { printf 'PROBE %s = %s\n' "$1" "$2"; }
have_cmd() { command -v "$1" >/dev/null 2>&1; }

# ---------- 0. 环境快照 ----------
echo "== 0. 环境快照 =="
probe XDG_CURRENT_DESKTOP "${XDG_CURRENT_DESKTOP:-<空>}"
probe KDE_FULL_SESSION "${KDE_FULL_SESSION:-<空>}"
probe KDE_SESSION_VERSION "${KDE_SESSION_VERSION:-<空>}"
probe XDG_SESSION_TYPE "${XDG_SESSION_TYPE:-<空>}"
probe WAYLAND_DISPLAY "${WAYLAND_DISPLAY:-<空>}"
probe DISPLAY "${DISPLAY:-<空>}"
if command -v plasmashell >/dev/null 2>&1; then
  probe plasmashell_version "$(plasmashell --version 2>&1 | head -1)"
fi
if command -v kwin_x11 >/dev/null 2>&1; then
  probe kwin_x11_version "$(kwin_x11 --version 2>&1 | head -1)"
fi
if command -v kwin_wayland >/dev/null 2>&1; then
  probe kwin_wayland_version "$(kwin_wayland --version 2>&1 | head -1)"
  # Q6：嵌套合成器支持（决定 CI 的 Wayland 档能不能跑）
  if kwin_wayland --help 2>&1 | grep -q -- '--virtual'; then
    probe kwin_wayland_virtual supported
  else
    probe kwin_wayland_virtual unsupported
  fi
else
  probe kwin_wayland_version "<missing>"
fi

# qdbus 名字在不同版本不同：5 系是 qdbus，Plasma 6 是 qdbus6 / qdbus-qt6
QDBUS=""
for c in qdbus6 qdbus qdbus-qt6 qdbus-qt5; do
  command -v "$c" >/dev/null 2>&1 && { QDBUS="$c"; break; }
done
probe qdbus_binary "${QDBUS:-<缺失>}"
if [ -z "$QDBUS" ]; then
  fail "没有 qdbus，后续 D-Bus 探针全部跳过（apt install qdbus-qt5 或 qdbus6）"
  echo "== 结果：前置缺失 ==" ; exit 2
fi

have_bus() { "$QDBUS" org.freedesktop.DBus / org.freedesktop.DBus.NameHasOwner "$1" 2>/dev/null | grep -q true; }

# ---------- 1. 会话总线上的 KDE 服务 ----------
echo
echo "== 1. 会话总线服务存在性 =="
for svc in org.kde.KWin org.kde.plasmashell org.kde.KScreen org.kde.kglobalaccel org.kde.StatusNotifierWatcher org.freedesktop.portal.Desktop; do
  if have_bus "$svc"; then probe "bus_$svc" present; else probe "bus_$svc" absent; fi
done

# ---------- Q1: evaluateScript 能否拿到 desktop containment 的 wallpaper 对象 ----------
echo
echo "== Q1. evaluateScript → desktop containment 的 wallpaper 对象（机制① A 路线）=="
if ! have_bus org.kde.plasmashell; then
  fail "org.kde.plasmashell 不在总线上，跳过 Q1"
else
  # 注意：evaluateScript 的返回值经 D-Bus 回传时经常是空字符串，所以用 print() 写自有日志更可靠
  # —— 这里两种都取：返回值 + 在脚本里把结果拼成字符串返回。
  Q1_JS='var out = [];
var ds = desktops();
out.push("desktops=" + ds.length);
if (ds.length > 0) {
  var d = ds[0];
  out.push("typeof_wallpaper=" + (typeof d.wallpaper));
  try { out.push("wallpaper_null=" + (d.wallpaper === null) + " undefined=" + (d.wallpaper === undefined)); } catch (e) { out.push("wallpaper_access_err=" + e); }
  try {
    if (d.wallpaper) {
      out.push("wp_visible=" + d.wallpaper.visible);
      out.push("wp_type=" + (typeof d.wallpaper));
      out.push("wp_plugin=" + d.wallpaper.plugin);
    }
  } catch (e) { out.push("wp_props_err=" + e); }
  try { out.push("containment_id=" + d.id + " formfactor=" + d.formfactor + " type=" + d.containmentType); } catch (e) { out.push("cont_err=" + e); }
  try { out.push("has_wallpaper_prop=" + (d.wallpaper !== undefined)); } catch (e) {}
}
out.join("|")'
  RAW="$("$QDBUS" org.kde.plasmashell /PlasmaShell org.kde.PlasmaShell.evaluateScript "$Q1_JS" 2>&1)"
  RC=$?
  if [ "$RC" != "0" ]; then
    fail "evaluateScript 调用失败 rc=$RC：$RAW"
    probe Q1_evaluateScript_error "$RAW"
  else
    info "evaluateScript 返回: ${RAW:-<空字符串>}"
    if [ -z "$RAW" ]; then
      warn "返回空 → 结论不能从返回值下；请另看 ~/.xsession-errors 或 plasmashell stdout 里的 print 输出"
      probe Q1_returns_empty yes
    else
      probe Q1_raw_result "$RAW"
      case "$RAW" in
        *wallpaper_null=false*|*wp_visible=*|*has_wallpaper_prop=true*)
          pass "能拿到 desktop containment 的 wallpaper 对象 → 机制① A 路线可行（继续验 visible=false 的效果）" ;;
        *wallpaper_null=true*|*wallpaper_null=true*)
          fail "d.wallpaper 为 null → 机制① A 路线不通，转透明 PNG 或机制②" ;;
        *)
          warn "返回值形态未知，需人工判读：$RAW" ;;
      esac
    fi
  fi
fi

# ---------- Q3: 桌面 containment 的配置键（机制②） ----------
echo
echo "== Q3. 桌面 containment 配置键（机制② 的可实施性）=="
APPLETSRC="$HOME/.config/plasma-org.kde.plasma.desktop-appletsrc"
if [ -f "$APPLETSRC" ]; then
  probe appletsrc_exists yes
  grep -nE '^\[Containments\]\[[0-9]+\]$|^plugin=|^formfactor=|^activityId=|^lastScreen=|^wallpaperplugin=' "$APPLETSRC" \
    | head -30 | while IFS= read -r l; do info "appletsrc: $l"; done
  probe appletsrc_containment_plugin "$(grep -m1 '^plugin=' "$APPLETSRC" | cut -d= -f2)"
  probe appletsrc_containment_formfactor "$(grep -m1 '^formfactor=' "$APPLETSRC" | cut -d= -f2)"
else
  probe appletsrc_exists no
  warn "还没有 appletsrc（桌面会话没跑过）→ 机制② 的键名要在真实会话里抓"
fi

# ---------- Q4: KWin 规则能否把普通窗口强制放进 DesktopLayer ----------
echo
echo "== Q4. KWin 规则 Layer=Desktop（Wayland 基线机制的可实施性）=="
if ! have_bus org.kde.KWin; then
  fail "org.kde.KWin 不在总线上，跳过 Q4"
else
  probe kwin_rules_interface "$("$QDBUS" org.kde.KWin /KWin org.freedesktop.DBus.Introspectable.Introspect 2>/dev/null | grep -oE '<interface name="[^"]+"' | sed 's/.*name="//;s/"//' | tr '\n' ',' )"
  # 用 KWin 脚本 API 读一个普通窗口的 resourceClass / layer / 可用属性名。
  # 关键：Plasma 5 的 org.kde.kwin.Scripting.loadScript 收的是**脚本目录**（目录里有
  # metadata.desktop + main.js），传 main.js 的路径会静默什么都不做。
  # 结果读法：脚本里的 print() 进 KWin 自身日志，qdbus 调用本身没有返回体，
  # 所以三处都查（journalctl --user / ~/.xsession-errors / kwin_x11 的 -t）。
  KWIN_JS='var out=[]; var cs=workspace.clientList(); out.push("clients="+cs.length);
if (cs.length>0) { var c=cs[0]; out.push("class="+c.resourceClass); out.push("layer="+c.layer); out.push("layer_undefined="+(c.layer===undefined));
  out.push("props="+Object.keys(c).join(",")); }
print("KWINPROBE " + out.join("|"));'
  RULES_KWIN_DIR="${TMPDIR:-/tmp}/kwinprobe.$$"
  mkdir -p "$RULES_KWIN_DIR"
  printf '%s\n' "$KWIN_JS" > "$RULES_KWIN_DIR/main.js"
  printf '[Desktop Entry]\nName=probe\nType=Service\nX-Plasma-API=javascript\nX-Plasma-MainScript=main.js\n' > "$RULES_KWIN_DIR/metadata.desktop"
  info "KWin 脚本目录: $RULES_KWIN_DIR"
  LOAD_OUT="$("$QDBUS" org.kde.KWin /Scripting org.kde.kwin.Scripting.loadScript "$RULES_KWIN_DIR" 2>&1)"
  info "loadScript 返回: ${LOAD_OUT:-<空>}（成功时通常返回脚本序号，如 1）"
  "$QDBUS" org.kde.KWin /Scripting org.kde.kwin.Scripting.start >/dev/null 2>&1
  sleep 2
  KWIN_LOG_HIT=""
  if have_cmd journalctl; then
    KWIN_LOG_HIT="$(journalctl --user --since '-2min' 2>/dev/null | grep -o 'KWINPROBE .*' | tail -1)"
    [ -z "$KWIN_LOG_HIT" ] && KWIN_LOG_HIT="$(journalctl --user -t kwin_x11 --no-pager 2>/dev/null | grep -o 'KWINPROBE .*' | tail -1)"
  fi
  if [ -z "$KWIN_LOG_HIT" ] && [ -f "$HOME/.xsession-errors" ]; then
    KWIN_LOG_HIT="$(grep -o 'KWINPROBE .*' "$HOME/.xsession-errors" 2>/dev/null | tail -1)"
  fi
  if [ -n "$KWIN_LOG_HIT" ]; then
    probe Q4_kwin_script_result "${KWIN_LOG_HIT#KWINPROBE }"
  else
    probe Q4_kwin_script_result "<未取到；查 journalctl --user | grep KWINPROBE 或 ~/.xsession-errors>"
    info "脚本可能执行了但日志未落盘；Q4 结论以下面的 queryWindowInfo 与二进制字符串取证为准"
  fi
  # queryWindowInfo：KWin 自带的窗口信息查询，不依赖脚本，能直接看到窗口 layer/类型
  if have_cmd xdotool; then
    WID="$(xdotool getactivewindow 2>/dev/null || true)"
    if [ -n "$WID" ]; then
      info "queryWindowInfo（活动窗口 $WID）: $("$QDBUS" org.kde.KWin /KWin org.kde.KWin.queryWindowInfo 2>&1 | head -c 400)"
    fi
  fi
  # 规则文件里 Layer 字段的存在性：直接看 KWin 的规则定义有没有 Layer
  if [ -f /etc/xdg/kwinrulesrc ] || [ -f "$HOME/.config/kwinrulesrc" ]; then
    info "已存在 kwinrulesrc；当前规则组："
    grep -E '^\[[0-9]+\]$' "$HOME/.config/kwinrulesrc" 2>/dev/null | head -5 | sed 's/^/         /'
  else
    probe kwinrulesrc_exists no
  fi
  # 层名与规则字段：直接从 kwin_x11 二进制的字符串表取证，与是否跑会话无关
  if have_cmd strings; then
    KWIN_BIN="$(command -v kwin_x11 2>/dev/null)"
    if [ -n "$KWIN_BIN" ] && [ -r "$KWIN_BIN" ]; then
      probe Q4_kwin_bin_layer_names "$(strings "$KWIN_BIN" 2>/dev/null | grep -xE 'DesktopLayer|BelowLayer|NormalLayer|AboveLayer|OverlayLayer' | sort -u | tr '\n' ',' )"
      probe Q4_kwin_bin_rule_keys "$(strings "$KWIN_BIN" 2>/dev/null | grep -xE 'Desktop|SkipTaskbar|SkipPager|SkipSwitcher|Layer|DecorationPolicy|Activity' | sort -u | tr '\n' ',' )"
    fi
  fi
fi

# ---------- 每屏查询（kscreen-doctor vs KScreen D-Bus） ----------
echo
echo "== 每屏查询数据源 =="
if command -v kscreen-doctor >/dev/null 2>&1; then
  KD_OUT="$(timeout 5 kscreen-doctor -j 2>&1 | head -c 400)"
  probe kscreen_doctor_json_ok "$([ -n "$KD_OUT" ] && echo yes || echo no)"
  info "kscreen-doctor -j 头 200 字符: ${KD_OUT:0:200}"
else
  probe kscreen_doctor missing
fi
if [ -f "$HOME/.config/kwinoutputconfig.json" ]; then
  probe kwinoutputconfig_exists yes
else
  probe kwinoutputconfig_exists no
fi

# ---------- 电源/空闲（自动暂停数据源） ----------
echo
echo "== 空闲/睡眠数据源 =="
for svc in org.freedesktop.ScreenSaver org.freedesktop.login1; do
  have_bus "$svc" && probe "idle_$svc" present || probe "idle_$svc" absent
done
if have_bus org.freedesktop.ScreenSaver; then
  probe screensaver_active "$("$QDBUS" org.freedesktop.ScreenSaver /ScreenSaver org.freedesktop.ScreenSaver.GetActive 2>&1)"
  probe screensaver_idle_ms "$("$QDBUS" org.freedesktop.ScreenSaver /ScreenSaver org.freedesktop.ScreenSaver.GetSessionIdleTime 2>&1)"
fi

# ---------- 静态壁纸 CLI ----------
echo
echo "== 静态壁纸同步手段 =="
for c in plasma-apply-wallpaperimage plasma-apply-desktoptheme qdbus6 qdbus; do
  command -v "$c" >/dev/null 2>&1 && probe "cli_$c" "$(command -v "$c")" || probe "cli_$c" missing
done

# ---------- 托盘 SNI ----------
echo
echo "== 托盘 =="
if have_bus org.kde.StatusNotifierWatcher; then
  probe sni_watcher present
  info "已注册托盘项: $("$QDBUS" org.kde.StatusNotifierWatcher /StatusNotifierWatcher org.kde.StatusNotifierWatcher.RegisteredStatusNotifierItems 2>&1 | head -c 300)"
else
  probe sni_watcher absent
fi

# ---------- 全局快捷键后端 ----------
echo
echo "== 全局快捷键 =="
if [ -n "${WAYLAND_DISPLAY:-}" ]; then
  if have_bus org.freedesktop.portal.Desktop; then
    probe portal_globalshortcuts "$("$QDBUS" org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop org.freedesktop.DBus.Introspectable.Introspect 2>/dev/null | grep -c GlobalShortcuts)"
  else
    probe portal_globalshortcuts no_portal
  fi
else
  probe x11_kglobalaccel "$(have_bus org.kde.kglobalaccel && echo present || echo absent)"
fi

echo
echo "== 结果 =="
if [ "$FAILED" -gt 0 ]; then
  warn "有 $FAILED 条探针判定为「不支持/失败」——这些正是要写进方案 §7 的结论"
fi
echo "把上面的 PROBE 行贴回 docs/kde-adaptation-plan.md §7，即可把「未验证」改成「已实测」。"
exit 0
