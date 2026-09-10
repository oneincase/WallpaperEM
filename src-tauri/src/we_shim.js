/**
 * WE 网页壁纸兼容 shim（注入到 iframe，必须在作者脚本之前执行）。
 *
 * 语料：本机库 42 张 web 壁纸扫描（2026-09）——
 *   wallpaperPropertyListener 29 / RegisterAudioListener 22 /
 *   RequestRandomFileForProperty 8 / userDirectoryFiles* 9 /
 *   Media*Listener 2 / PluginListener 2
 *
 * 官方 CEF 在任何壁纸脚本前就把这些做成原生函数；工坊顶层直接注册。
 * 本文件作为 <head> 首个 classic script 插入。
 *
 * 父页控制面 __we*（main.ts weShimCall / web.ts 泵）：
 *   __weSetPaused / __weSetFps / __weSetVolume / __weApplyProps / __weSeedProps
 *   __wePushAudio(arr128)
 *   __wePushMedia(event)  — {op, payload} 见下
 *   __wePushDirectoryFiles(prop, files) / __weRemoveDirectoryFiles(prop, files)
 *   __weRewriteFileUrl(s) — file:/// → 同源相对（HTTP 页）；空 file:/// → ""
 *   __wePushPointer(x, y, buttons, mods) / __wePointerLeave() — 外部指针注入（见文末）
 *   __wePushWheel(x, y, dx, dy, mode, mods) — 外部滚轮注入（含触摸板捏合，见文末）
 */
(function (w) {
  "use strict";
  try {
    if (w.document && w.document.documentElement) {
      w.document.documentElement.setAttribute("data-we-shim", "1");
    }
  } catch (_) {
    /* 忽略 */
  }

  var audioListener = null;
  var propertyListener = null;
  var paused = false;
  var fps = 60;
  var volume = 0;
  var pendingProps = null;
  var pendingGeneral = null;
  var rafMap = Object.create(null);
  var rafCounter = 0;
  var origRaf = w.requestAnimationFrame.bind(w);
  var origCaf = w.cancelAnimationFrame.bind(w);

  // 官方 CEF 以文件系统为源，作者普遍 `'file:///' + value`。HTTP 同源页里
  // file:///files/x.webm 加载失败；空 value 变成 file:///（1748506393）。
  // file: 协议页保持原样（真本地嵌入）。
  function rewriteBareFileUrl(url) {
    if (typeof url !== "string") return url;
    var s = url.trim();
    if (!/^file:/i.test(s)) return s;
    try {
      var loc = w.location;
      if (loc && loc.protocol === "file:") return s;
    } catch (_) {
      /* 忽略 */
    }
    var rest = s.replace(/^file:\/\//i, "").replace(/^\/+/, "");
    if (!rest) return "";
    if (/^[a-zA-Z][:|]/.test(rest)) return "";
    if (/^(Users|home|tmp|var|etc|private|Volumes)\//.test(rest)) return "";
    try {
      var href = w.location && w.location.href;
      if (href) return new URL(rest, href).href;
    } catch (_) {
      /* 忽略 */
    }
    return rest;
  }

  function rewriteWeFileUrl(input) {
    if (typeof input !== "string") return input;
    if (/url\(/i.test(input)) {
      return input.replace(/url\(\s*(['"]?)([^)'"]*?)\1\s*\)/gi, function (_m, q, inner) {
        var next = rewriteBareFileUrl(inner);
        if (!next) return "none";
        var quote = q || '"';
        return "url(" + quote + next + quote + ")";
      });
    }
    return rewriteBareFileUrl(input);
  }

  w.__weRewriteFileUrl = rewriteWeFileUrl;

  function installFileUrlHooks() {
    try {
      if (w.Element && w.Element.prototype && typeof w.Element.prototype.setAttribute === "function") {
        var origSetAttr = w.Element.prototype.setAttribute;
        w.Element.prototype.setAttribute = function (name, value) {
          var n = String(name || "").toLowerCase();
          if (n === "src" || n === "href" || n === "poster") value = rewriteWeFileUrl(value);
          return origSetAttr.call(this, name, value);
        };
      }
    } catch (_) {
      /* 无 DOM 的 verifier 跳过 */
    }
    var ctorNames = ["HTMLImageElement", "HTMLMediaElement", "HTMLSourceElement", "HTMLScriptElement"];
    for (var i = 0; i < ctorNames.length; i++) {
      try {
        var Ctor = w[ctorNames[i]];
        if (!Ctor || !Ctor.prototype) continue;
        var desc = Object.getOwnPropertyDescriptor(Ctor.prototype, "src");
        if (!desc || typeof desc.set !== "function") continue;
        (function (d) {
          Object.defineProperty(Ctor.prototype, "src", {
            configurable: true,
            enumerable: d.enumerable,
            get: d.get,
            set: function (v) {
              d.set.call(this, rewriteWeFileUrl(v));
            },
          });
        })(desc);
      } catch (_) {
        /* 忽略单个原型 */
      }
    }
    try {
      var styleDesc =
        w.HTMLElement && Object.getOwnPropertyDescriptor(w.HTMLElement.prototype, "style");
      // Chromium 的 backgroundImage 不是原型自有描述符，只能包 HTMLElement.style 的 Proxy。
      if (styleDesc && typeof styleDesc.get === "function" && w.Proxy && w.WeakMap) {
        var styleCache = new w.WeakMap();
        Object.defineProperty(w.HTMLElement.prototype, "style", {
          configurable: true,
          enumerable: styleDesc.enumerable,
          get: function () {
            var raw = styleDesc.get.call(this);
            if (!raw) return raw;
            var cached = styleCache.get(raw);
            if (cached) return cached;
            var proxy = new w.Proxy(raw, {
              set: function (target, prop, value) {
                if (typeof value === "string" && typeof prop === "string" && /background/i.test(prop)) {
                  value = rewriteWeFileUrl(value);
                }
                target[prop] = value;
                return true;
              },
              get: function (target, prop) {
                var v = target[prop];
                if (typeof v === "function") return v.bind(target);
                return v;
              },
            });
            styleCache.set(raw, proxy);
            return proxy;
          },
          set: styleDesc.set,
        });
      }
      var styleProto = w.CSSStyleDeclaration && w.CSSStyleDeclaration.prototype;
      if (styleProto && typeof styleProto.setProperty === "function") {
        var origSetProp = styleProto.setProperty;
        styleProto.setProperty = function (name, value, priority) {
          if (typeof value === "string" && /background/i.test(String(name || ""))) {
            value = rewriteWeFileUrl(value);
          }
          return origSetProp.call(this, name, value, priority);
        };
      }
    } catch (_) {
      /* 忽略 */
    }
  }
  installFileUrlHooks();

  // propertyName → string[]（绝对/相对路径；随机文件从此抽）
  var directoryFiles = Object.create(null);

  var mediaListeners = {
    properties: null,
    thumbnail: null,
    playback: null,
    timeline: null,
    status: null,
  };
  // 晚注册时回放最近一帧（作者脚本常在 DOMContentLoaded 后才 Register）
  var lastMedia = {
    properties: null,
    thumbnail: null,
    playback: null,
    timeline: null,
    status: null,
  };

  function callApplyUserProperties(props) {
    if (!propertyListener || typeof propertyListener.applyUserProperties !== "function") return;
    try {
      propertyListener.applyUserProperties(props || {});
    } catch (_) {
      /* 壁纸脚本抛错不打断宿主 */
    }
  }

  function callApplyGeneralProperties(props) {
    if (!propertyListener || typeof propertyListener.applyGeneralProperties !== "function") return;
    try {
      propertyListener.applyGeneralProperties(props || {});
    } catch (_) {
      /* 忽略 */
    }
  }

  function callSetPaused(v) {
    if (!propertyListener || typeof propertyListener.setPaused !== "function") return;
    try {
      propertyListener.setPaused(!!v);
    } catch (_) {
      /* 忽略 */
    }
  }

  function callDirectoryAdded(prop, files) {
    if (!propertyListener || typeof propertyListener.userDirectoryFilesAddedOrChanged !== "function")
      return;
    try {
      propertyListener.userDirectoryFilesAddedOrChanged(prop, files);
    } catch (_) {
      /* 忽略 */
    }
  }

  function callDirectoryRemoved(prop, files) {
    if (!propertyListener || typeof propertyListener.userDirectoryFilesRemoved !== "function") return;
    try {
      propertyListener.userDirectoryFilesRemoved(prop, files);
    } catch (_) {
      /* 忽略 */
    }
  }

  function flushPending() {
    if (pendingProps) {
      var p = pendingProps;
      pendingProps = null;
      callApplyUserProperties(p);
    }
    if (pendingGeneral) {
      var g = pendingGeneral;
      pendingGeneral = null;
      callApplyGeneralProperties(g);
    }
  }

  /**
   * 工坊常在 React render 里写 `window.wallpaperPropertyListener = {…}`（2905017768）。
   * 官方 CEF 不会在赋值当下同步回调 setPaused/apply*；若我们同步 flush，
   * 等于 render 中 setState → React 熔断 → #root 空（一片黑）。
   */
  function afterAssign(fn) {
    try {
      if (typeof w.queueMicrotask === "function") w.queueMicrotask(fn);
      else w.setTimeout(fn, 0);
    } catch (_) {
      try {
        fn();
      } catch (_) {
        /* 忽略 */
      }
    }
  }

  function safeCall(fn, arg) {
    if (typeof fn !== "function") return;
    try {
      fn(arg);
    } catch (_) {
      /* 忽略 */
    }
  }

  // —— 媒体集成枚举（3747222633：缺省时 PLAYBACK_PLAYING||0 会把「播放」当成 0）——
  w.wallpaperMediaIntegration = {
    PLAYBACK_STOPPED: 0,
    PLAYBACK_PLAYING: 1,
    PLAYBACK_PAUSED: 2,
  };

  // —— 官方 API：音频 ——
  w.wallpaperRegisterAudioListener = function (cb) {
    audioListener = typeof cb === "function" ? cb : null;
  };

  // —— 官方 API：媒体 ——
  w.wallpaperRegisterMediaPropertiesListener = function (cb) {
    mediaListeners.properties = typeof cb === "function" ? cb : null;
    if (mediaListeners.properties && lastMedia.properties) {
      safeCall(mediaListeners.properties, lastMedia.properties);
    }
  };
  w.wallpaperRegisterMediaThumbnailListener = function (cb) {
    mediaListeners.thumbnail = typeof cb === "function" ? cb : null;
    if (mediaListeners.thumbnail && lastMedia.thumbnail) {
      safeCall(mediaListeners.thumbnail, lastMedia.thumbnail);
    }
  };
  w.wallpaperRegisterMediaPlaybackListener = function (cb) {
    mediaListeners.playback = typeof cb === "function" ? cb : null;
    if (mediaListeners.playback && lastMedia.playback) {
      safeCall(mediaListeners.playback, lastMedia.playback);
    }
  };
  w.wallpaperRegisterMediaTimelineListener = function (cb) {
    mediaListeners.timeline = typeof cb === "function" ? cb : null;
    if (mediaListeners.timeline && lastMedia.timeline) {
      safeCall(mediaListeners.timeline, lastMedia.timeline);
    }
  };
  w.wallpaperRegisterMediaStatusListener = function (cb) {
    mediaListeners.status = typeof cb === "function" ? cb : null;
    if (mediaListeners.status && lastMedia.status) {
      safeCall(mediaListeners.status, lastMedia.status);
    }
  };

  // —— 官方 API：随机文件（slideshow）——
  // 回调签名：function(propertyName, filePath)。无库存文件时 filePath 为空串（语料 if(i) 守卫）。
  w.wallpaperRequestRandomFileForProperty = function (propertyName, callback) {
    if (typeof callback !== "function") return;
    var prop = String(propertyName || "");
    var list = directoryFiles[prop];
    var path = "";
    if (list && list.length) {
      path = String(list[(Math.random() * list.length) | 0] || "");
    }
    try {
      callback(prop, path);
    } catch (_) {
      /* 忽略 */
    }
  };

  // —— PropertyListener（getter/setter；回调延后到微任务，见 afterAssign）——
  // 官方在页面加载完成后才发全量属性/暂停状态；首屏脚本（body onLoad=init 等）常
  // 假设属性到达时 DOM/场景已初始化（827982449：applyUserProperties→cl() 在 load 前
  // 跑会撞上未创建的 scene/material）。未加载完成时等 window load + 一个宏任务
  // （保证排在 onLoad 属性处理器之后），已加载完成则微任务即发。
  function whenPageReady(fn) {
    var ready = "complete";
    try {
      ready = w.document.readyState;
    } catch (_) {
      /* 忽略 */
    }
    if (ready === "complete") {
      afterAssign(fn);
      return;
    }
    try {
      w.addEventListener("load", function () {
        // setTimeout 保证排在 load 同步链（onLoad 处理器）之后
        w.setTimeout(fn, 0);
      }, { once: true });
    } catch (_) {
      afterAssign(fn);
    }
  }
  Object.defineProperty(w, "wallpaperPropertyListener", {
    configurable: true,
    enumerable: true,
    get: function () {
      return propertyListener;
    },
    set: function (v) {
      var next = v && typeof v === "object" ? v : null;
      var prev = propertyListener;
      propertyListener = next;
      if (!next) return;
      // 仅首次注册补发挂载状态。官方 CEF 从不在赋值当下回调；2905017768 等 React 壁纸在
      // 渲染体里重新赋值（新对象字面量），若每次都补 setPaused 会形成
      // 渲染 → 赋值 → 补发 setState → 再渲染 的微任务死循环（点下一曲整页卡死）。
      if (prev) return;
      whenPageReady(function () {
        flushPending();
        callApplyGeneralProperties({ fps: fps });
        callSetPaused(paused);
        // 已缓存的目录文件补推一次（作者可能后挂 userDirectoryFilesAddedOrChanged）
        for (var prop in directoryFiles) {
          if (Object.prototype.hasOwnProperty.call(directoryFiles, prop) && directoryFiles[prop].length) {
            callDirectoryAdded(prop, directoryFiles[prop].slice());
          }
        }
      });
    },
  });

  // —— Plugin（iCUE 等；无硬件时空实现，避免 if 判断失败）——
  if (!w.wallpaperPluginListener) {
    w.wallpaperPluginListener = {
      onPluginLoaded: function () {},
    };
  }

  // —— 定时器冻结：官方暂停 = "fully freeze the process that renders the wallpaper"，
  // rAF 已在节流层挂起，这里冻结定时器：暂停期间新建的挂起登记、恢复时按原延迟/间隔
  // 重新启动；**已启动**的真定时器到期由包装回调拦下——timeout 转挂起（恢复后立即补跑，
  // 近似官方的剩余等待），interval 直接跳过该周期（恢复后从下个周期继续）。
  var pendTimers = [];
  var tmSeq = 0;
  var TM_BASE = 0x40000000; // 假 id 段，避免与真实 timer id 混淆
  var origST = w.setTimeout;
  var origSI = w.setInterval;
  var origCTO = w.clearTimeout;
  var origCIT = w.clearInterval;
  function guardTimeout(fn) {
    if (typeof fn !== "function") return fn;
    return function () {
      if (paused) {
        pendTimers.push({ id: 0, kind: "t", fn: fn, ms: 1, extra: [] });
        return;
      }
      return fn.apply(this, arguments);
    };
  }
  function guardInterval(fn) {
    if (typeof fn !== "function") return fn;
    return function () {
      if (paused) return;
      return fn.apply(this, arguments);
    };
  }
  function startTimer(kind, fn, ms, extra) {
    if (paused) {
      var id = TM_BASE + ++tmSeq;
      pendTimers.push({ id: id, kind: kind, fn: fn, ms: ms, extra: extra });
      return id;
    }
    var args = [kind === "t" ? guardTimeout(fn) : guardInterval(fn), ms].concat(extra);
    return (kind === "t" ? origST : origSI).apply(w, args);
  }
  w.setTimeout = function (fn, ms) {
    return startTimer("t", fn, ms, Array.prototype.slice.call(arguments, 2));
  };
  w.setInterval = function (fn, ms) {
    return startTimer("i", fn, ms, Array.prototype.slice.call(arguments, 2));
  };
  function unpend(id) {
    for (var i = 0; i < pendTimers.length; i++) {
      if (pendTimers[i].id === id) {
        pendTimers.splice(i, 1);
        return true;
      }
    }
    return false;
  }
  w.clearTimeout = function (id) {
    if (unpend(id)) return;
    origCTO.call(w, id);
  };
  w.clearInterval = function (id) {
    if (unpend(id)) return;
    origCIT.call(w, id);
  };
  function resumeTimers() {
    var list = pendTimers;
    pendTimers = [];
    for (var i = 0; i < list.length; i++) {
      var t = list[i];
      (t.kind === "t" ? origST : origSI).call(w, t.fn, t.ms, t.extra);
    }
  }

  /**
   * 恢复暂停期间被挂起的 rAF 请求。
   *
   * **不补跑这些回调，主循环就永久断掉**（1278092907 Monstercat：`draw()` 在函数体
   * 开头就 `requestAnimationFrame(draw)` 再画，暂停期间那次请求被登记成 hold，
   * 恢复后没人跑它 → 整条链没有下一帧，画面永久定格，且没有任何报错）。
   * 这是 rAF 自递归的通用形态，不是这一张的特例。
   *
   * 走 `w.requestAnimationFrame` 而不是 `origRaf`：此时已 unpaused，要让它重新经过
   * 节流层（低 fps 时该走 setTimeout 路径），并照常发 we-frame 打点。
   */
  function resumeRafHolds() {
    var holds = [];
    for (var id in rafMap) {
      if (!Object.prototype.hasOwnProperty.call(rafMap, id)) continue;
      if (rafMap[id] && rafMap[id].kind === "hold") {
        holds.push(rafMap[id].cb);
        delete rafMap[id];
      }
    }
    for (var i = 0; i < holds.length; i++) {
      try {
        w.requestAnimationFrame(holds[i]);
      } catch (_) {
        /* 单个回调重挂失败不影响其它 */
      }
    }
  }

  // —— 媒体音量：对齐官方 CEF 语义（浏览器级主音量与作者页面内音量独立相乘）——
  // 作者常在播放前重设 a.volume = uiVolume（Bocchi），且音频多为 `new Audio()` 不进
  // DOM——querySelectorAll 找不到、直接覆盖 volume 又会被作者回写。因此 hook 原型：
  // setter 记作者值，元素实际音量 = 作者值 × 主音量；`__weSetVolume` 改系数并刷新
  // 全部活实例（DOM 内 + Audio 构造器登记的 WeakRef）。
  var hostVolume = 1;
  var liveMedia = []; // WeakRef<HTMLMediaElement>
  function trackMedia(el) {
    if (!w.WeakRef) return;
    liveMedia.push(new w.WeakRef(el));
  }
  function applyMediaVolume(el) {
    if (el.__weBaseVol != null) {
      mediaVolDesc.set.call(el, el.__weBaseVol * hostVolume);
    } else {
      mediaVolDesc.set.call(el, hostVolume);
    }
    var baseMuted = !!el.__weBaseMuted;
    mediaMutedDesc.set.call(el, baseMuted || hostVolume <= 0);
  }
  function refreshAllMediaVolume() {
    try {
      var nodes = w.document.querySelectorAll("audio,video");
      for (var i = 0; i < nodes.length; i++) applyMediaVolume(nodes[i]);
    } catch (_) {
      /* 忽略 */
    }
    for (var j = liveMedia.length - 1; j >= 0; j--) {
      var el = liveMedia[j].deref();
      if (!el) {
        liveMedia.splice(j, 1);
        continue;
      }
      applyMediaVolume(el);
    }
  }
  var mediaVolDesc = null;
  var mediaMutedDesc = null;
  function installMediaVolumeHooks() {
    try {
      if (!w.HTMLMediaElement || !w.HTMLMediaElement.prototype) return;
      var proto = w.HTMLMediaElement.prototype;
      mediaVolDesc = Object.getOwnPropertyDescriptor(proto, "volume");
      mediaMutedDesc = Object.getOwnPropertyDescriptor(proto, "muted");
      if (mediaVolDesc && typeof mediaVolDesc.set === "function") {
        Object.defineProperty(proto, "volume", {
          configurable: true,
          enumerable: mediaVolDesc.enumerable,
          get: function () {
            return this.__weBaseVol != null ? this.__weBaseVol : mediaVolDesc.get.call(this);
          },
          set: function (v) {
            this.__weBaseVol = Math.max(0, Math.min(1, Number(v) || 0));
            mediaVolDesc.set.call(this, this.__weBaseVol * hostVolume);
          },
        });
      }
      if (mediaMutedDesc && typeof mediaMutedDesc.set === "function") {
        Object.defineProperty(proto, "muted", {
          configurable: true,
          enumerable: mediaMutedDesc.enumerable,
          get: function () {
            return this.__weBaseMuted != null
              ? this.__weBaseMuted || hostVolume <= 0
              : mediaMutedDesc.get.call(this);
          },
          set: function (v) {
            this.__weBaseMuted = !!v;
            mediaMutedDesc.set.call(this, !!v || hostVolume <= 0);
          },
        });
      }
      // `new Audio()` 不进 DOM：构造器登记 WeakRef 以便主音量变化时刷新
      if (typeof w.Audio === "function" && w.WeakRef) {
        var OrigAudio = w.Audio;
        function WrappedAudio(src) {
          var a = new OrigAudio(src);
          trackMedia(a);
          applyMediaVolume(a);
          return a;
        }
        WrappedAudio.prototype = OrigAudio.prototype;
        w.Audio = WrappedAudio;
      }
    } catch (_) {
      /* 无媒体环境的 verifier 跳过 */
    }
  }
  installMediaVolumeHooks();

  // —— 父页控制面 ——
  // 官方 setPaused 只在暂停状态实际变化时调用一次；重复调用去重。
  // 暂停还要冻结页内媒体：官方是进程级冻结（无声、解码器可回收），作者的
  // setPaused 常只管自己的逻辑。只记录「我们代为暂停」的元素，恢复时仅还原这部分，
  // 不碰作者自己暂停的。
  var weFrozenMedia = [];
  function freezePageMedia() {
    weFrozenMedia.length = 0;
    try {
      var nodes = w.document.querySelectorAll("audio,video");
      for (var i = 0; i < nodes.length; i++) {
        if (!nodes[i].paused) {
          weFrozenMedia.push(nodes[i]);
          try {
            nodes[i].pause();
          } catch (_) {
            /* 忽略 */
          }
        }
      }
    } catch (_) {
      /* 忽略 */
    }
  }
  function thawPageMedia() {
    for (var i = 0; i < weFrozenMedia.length; i++) {
      try {
        var p = weFrozenMedia[i].play();
        if (p && p.catch) p.catch(function () {});
      } catch (_) {
        /* 忽略 */
      }
    }
    weFrozenMedia.length = 0;
  }

  /**
   * 暂停还要冻结 **CSS 动画 / 过渡**（Web Animations 时间轴）。
   *
   * rAF 与定时器冻结管不到它们：CSS `animation` 由浏览器**合成器**独立驱动，
   * 与 JS 主线程无关。1444432396 Glitch Clock 的整个视觉（背景移动、抖动、故障
   * 闪烁）是 10 处 `animation: … infinite`，只有时钟文字走 `setInterval` ——
   * 暂停后画面照旧动个不停，用户看到的就是「无法暂停」（实测暂停期间 6 个动画
   * 全为 `playState:"running"`，`currentTime` 700ms 推进整 700ms）。
   *
   * 官方暂停语义是「fully freeze the process that renders the wallpaper」，
   * 合成器动画自然也在冻结范围内。
   *
   * 与媒体冻结同一条纪律：**只记录我们代为暂停的**，恢复时仅还原这部分——
   * 作者自己用 `animation-play-state: paused` 停下的（常见于 hover 才播的装饰）
   * 不能被我们唤醒。`getAnimations()` 拿的是活动动画对象，`pause()`/`play()`
   * 直接作用在时间轴上，比改 `style.animationPlayState` 干净（后者会污染作者的
   * 内联样式，且被作者下一次样式写入覆盖）。
   */
  var weFrozenAnims = [];
  function freezePageAnimations() {
    weFrozenAnims.length = 0;
    try {
      if (typeof w.document.getAnimations !== "function") return;
      var anims = w.document.getAnimations();
      for (var i = 0; i < anims.length; i++) {
        var a = anims[i];
        if (a && a.playState === "running") {
          weFrozenAnims.push(a);
          try {
            a.pause();
          } catch (_) {
            /* 个别动画不可暂停时跳过 */
          }
        }
      }
    } catch (_) {
      /* 旧引擎无 getAnimations：退化为不冻结，不报错 */
    }
  }
  function thawPageAnimations() {
    for (var i = 0; i < weFrozenAnims.length; i++) {
      try {
        weFrozenAnims[i].play();
      } catch (_) {
        /* 已被作者移除的动画忽略 */
      }
    }
    weFrozenAnims.length = 0;
  }
  w.__weSetPaused = function (v) {
    var next = !!v;
    if (next === paused) return;
    paused = next;
    if (paused) {
      callSetPaused(true);
      freezePageMedia();
      freezePageAnimations();
    } else {
      callSetPaused(false);
      thawPageMedia();
      thawPageAnimations();
      resumeTimers();
      // rAF 挂起项必须补跑，否则自递归的主循环永久断链（1278092907）
      resumeRafHolds();
    }
  };

  w.__weSetFps = function (n) {
    var next = Number(n);
    if (!Number.isFinite(next) || next <= 0) return;
    fps = next;
    callApplyGeneralProperties({ fps: fps });
  };

  w.__weSetVolume = function (v) {
    var next = Math.max(0, Math.min(1, Number(v) || 0));
    volume = next;
    hostVolume = next;
    refreshAllMediaVolume();
  };

  w.__weApplyProps = function (props) {
    if (!props || typeof props !== "object") return;
    // file 属性：值是路径时登记进随机池（单文件 slideshow）
    try {
      for (var key in props) {
        if (!Object.prototype.hasOwnProperty.call(props, key)) continue;
        var ent = props[key];
        var val = ent && typeof ent === "object" && "value" in ent ? ent.value : ent;
        if (typeof val === "string" && val !== "" && /\.(png|jpe?g|gif|webp|webm|mp4|bmp)$/i.test(val)) {
          directoryFiles[key] = [val];
        }
      }
    } catch (_) {
      /* 忽略 */
    }
    if (!propertyListener || typeof propertyListener.applyUserProperties !== "function") {
      pendingProps = props;
      return;
    }
    callApplyUserProperties(props);
  };

  w.__weSeedProps = function (props) {
    if (!props || typeof props !== "object") return;
    if (propertyListener && typeof propertyListener.applyUserProperties === "function") {
      w.__weApplyProps(props);
    } else {
      pendingProps = props;
    }
  };

  w.__wePushAudio = function (arr) {
    if (paused || !audioListener) return;
    try {
      audioListener(arr);
    } catch (_) {
      /* 忽略 */
    }
  };

  /**
   * 媒体事件泵。payload 形态对齐官方：
   *   { op:"properties", title, artist, album, albumArtist }
   *   { op:"thumbnail", thumbnail, primaryColor, textColor, ... }
   *   { op:"playback", state }  // 0/1/2
   *   { op:"timeline", position, duration }
   *   { op:"status", enabled }
   */
  w.__wePushMedia = function (payload) {
    if (!payload || typeof payload !== "object") return;
    var op = payload.op;
    if (op === "properties") {
      lastMedia.properties = payload;
      safeCall(mediaListeners.properties, payload);
    } else if (op === "thumbnail") {
      lastMedia.thumbnail = payload;
      safeCall(mediaListeners.thumbnail, payload);
    } else if (op === "playback") {
      lastMedia.playback = payload;
      safeCall(mediaListeners.playback, payload);
    } else if (op === "timeline") {
      lastMedia.timeline = payload;
      safeCall(mediaListeners.timeline, payload);
    } else if (op === "status") {
      lastMedia.status = payload;
      safeCall(mediaListeners.status, payload);
    }
  };

  /** 目录文件列表（首次或追加）。files: string[] */
  w.__wePushDirectoryFiles = function (propertyName, files) {
    var prop = String(propertyName || "");
    if (!prop || !Array.isArray(files)) return;
    var cleaned = [];
    for (var i = 0; i < files.length; i++) {
      if (files[i] != null && String(files[i]) !== "") cleaned.push(String(files[i]));
    }
    if (!directoryFiles[prop]) directoryFiles[prop] = [];
    // 首次全量替换语义由调用方决定；这里 concat 去重
    var seen = Object.create(null);
    for (var j = 0; j < directoryFiles[prop].length; j++) seen[directoryFiles[prop][j]] = 1;
    var added = [];
    for (var k = 0; k < cleaned.length; k++) {
      if (!seen[cleaned[k]]) {
        seen[cleaned[k]] = 1;
        directoryFiles[prop].push(cleaned[k]);
        added.push(cleaned[k]);
      }
    }
    if (added.length) callDirectoryAdded(prop, added);
  };

  w.__weRemoveDirectoryFiles = function (propertyName, files) {
    var prop = String(propertyName || "");
    if (!prop || !Array.isArray(files) || !directoryFiles[prop]) return;
    var removeSet = Object.create(null);
    for (var i = 0; i < files.length; i++) removeSet[String(files[i])] = 1;
    var kept = [];
    var removed = [];
    for (var j = 0; j < directoryFiles[prop].length; j++) {
      var f = directoryFiles[prop][j];
      if (removeSet[f]) removed.push(f);
      else kept.push(f);
    }
    directoryFiles[prop] = kept;
    if (removed.length) callDirectoryRemoved(prop, removed);
  };

  // —— 外部指针注入（桌面 underlay 层收不到鼠标事件，父页经 __wp.pushPointer 推入）——
  //
  // 场景壁纸那条通道是「写一个状态对象、渲染器每帧读」（render/pointer.js）；网页壁纸
  // 没有这样的单一消费点 —— 作者代码就是**监听 DOM 事件**的，所以这里必须把推送
  // 还原成一串合成事件。语料（本机 49 张 web）：mousemove 24 张、click 29 张、
  // mouseover/out 17 张、mouseenter/leave 8 张、pointer* 16 张（createjs 系一律走
  // pointerdown/move/up）、.button 18 张、.which 17 张、pointerId/relatedTarget 15 张。
  //
  // 三条要点（都有语料依据，改错了会静默失效）：
  //
  //   1. **必须 elementFromPoint 按命中元素派发**，不能一律打 document。作者既有挂
  //      document/window 的（15 张，靠冒泡收到），也有挂 canvas 上读 `event.offsetX`
  //      的（1748506393 流体 `pointers[0].dx = (e.offsetX - …)`）。offsetX/offsetY 由
  //      浏览器按 target 的 padding box 现算 —— target 打错就是错的偏移，且无任何报错。
  //      pageX/pageY 同理由 clientX + 滚动量现算，不用我们填。
  //
  //   2. **over/out/enter/leave 链要按 W3C 语义补全**。1748506393 靠 canvas 的
  //      `mouseenter` 把 `pointers[0].down` 置 true（不进这个分支则鼠标怎么动都不出染料）、
  //      靠 window 的 `mouseleave` 复位；1081733658 animatedGrid 靠 `document.body` 的
  //      mouseover/mouseleave 起停整个网格动画。leave/enter 不冒泡，必须自己沿祖先链走到
  //      最近公共祖先，只发生变化的那一段。
  //
  //   3. **click 要靠 down/up 边缘合成**，且 down 与 up 的 target 不同（拖拽）时不发。
  //      29 张听 click 是最大的消费方；轮询推送里没有「点击」这个事件，只有按键掩码的
  //      跳变，边缘丢了就等于整类交互消失。
  //
  // 硬限制（写在这里避免反复试）：CSS `:hover` 由浏览器自己的 hit-test 驱动，合成事件
  // 永远点不亮它（18 张含 `:hover`）—— 纯 CSS hover 动画的壁纸无法用注入通道响应，
  // 这不是实现缺陷，是合成事件的固有边界。
  var ptrHas = false; // 是否收到过推送（首帧 movement 归零用）
  var ptrX = 0;
  var ptrY = 0;
  var ptrButtons = 0;
  var ptrTarget = null; // 上次命中元素（over/out 链的旧端）
  var ptrDownTarget = null; // 按下时的命中元素（click 判定）
  var ptrLastClickTime = 0;
  var ptrLastClickTarget = null;
  /**
   * 修饰键掩码：bit0 ctrl / bit1 shift / bit2 alt / bit3 meta。
   *
   * 由 __wePushPointer 与 __wePushWheel 的末位参数共同维护（宿主知道当前键盘状态，
   * 谁后推谁赢）。省略参数时归 0 —— 老宿主不传就等于改动前的全 false，无回归。
   *
   * 为什么必须有：触摸板双指捏合在浏览器里就是「ctrlKey 为真的 wheel」，
   * OrbitControls / pano2vr 都靠 event.ctrlKey 把缩放和滚动分开。掩码写死 false
   * 时捏合与普通滚动无从区分。
   */
  var ptrMods = 0;
  /** 双击判定窗口（ms）。与主流浏览器一致，语料里 5 张听 dblclick。 */
  var PTR_DBLCLICK_MS = 500;

  function ptrRoot() {
    try {
      return w.document.body || w.document.documentElement || null;
    } catch (_) {
      return null;
    }
  }

  function ptrHitTest(x, y) {
    try {
      if (typeof w.document.elementFromPoint === "function") {
        var el = w.document.elementFromPoint(x, y);
        if (el) return el;
      }
    } catch (_) {
      /* 忽略 */
    }
    return ptrRoot();
  }

  /** node → [node, parent, …, root]；用 parentNode 而非 parentElement，
   *  这样 document / documentElement 也在链里（作者挂 document 的 leave 要收到）。 */
  function ptrChain(node) {
    var out = [];
    var n = node;
    while (n) {
      out.push(n);
      try {
        n = n.parentNode || null;
      } catch (_) {
        n = null;
      }
    }
    return out;
  }

  function ptrCommonAncestor(a, b) {
    if (!a || !b) return null;
    var ca = ptrChain(a);
    var seen = [];
    for (var i = 0; i < ca.length; i++) seen.push(ca[i]);
    var cb = ptrChain(b);
    for (var j = 0; j < cb.length; j++) {
      for (var k = 0; k < seen.length; k++) {
        if (seen[k] === cb[j]) return cb[j];
      }
    }
    return null;
  }

  /**
   * 造一个合成鼠标/指针事件。
   *
   * `PointerEvent` 优先：createjs 一族（语料 7 张）只挂 pointerdown/move/up，
   * 且会读 `pointerId` / `pointerType` / `isPrimary`。环境没有 PointerEvent 时
   * 退回 MouseEvent（事件名照旧，作者的 addEventListener('pointermove') 仍能收到）。
   */
  function ptrMakeEvent(type, x, y, opts) {
    var o = opts || {};
    var isPointer = type.indexOf("pointer") === 0;
    var init = {
      bubbles: o.bubbles !== false,
      cancelable: o.cancelable !== false,
      // composed：作者把 canvas 放进 shadow DOM 时事件要能穿出来
      composed: true,
      view: w,
      detail: o.detail || 0,
      clientX: x,
      clientY: y,
      // screenX/screenY 是 init 字段（不像 pageX/offsetX 那样现算）。iframe 里
      // 只能按外层窗口原点近似；16 张读 screenX，多用于算相对位移而非绝对定位。
      screenX: x + (Number(w.screenX) || 0),
      screenY: y + (Number(w.screenY) || 0),
      // button：**移动/悬停类事件必须是 -1**，只有 down/up/click 才是 0（左）/1（中）/2（右）。
      // 这条是 W3C 规定的「没有按键状态变化」哨兵值，不是可省的细节：GameMaker HTML5
      // 导出的运行时（2517518192 FNAF）在 pointermove 分支里照抄 `_tq = e.button` 再
      // `_mq |= (1 << _tq)`，而 _mq 只在 pointerup/out 才清零 —— 填 0 等于告诉游戏
      // 「左键一直按着」，鼠标只是移过去就永久卡在按下态（且没有任何报错）。
      button: o.button != null ? o.button : -1,
      buttons: o.buttons != null ? o.buttons : ptrButtons,
      movementX: o.movementX || 0,
      movementY: o.movementY || 0,
      // 修饰键由宿主推送的掩码驱动（见 ptrMods）。曾经硬编码 false，
      // 于是触摸板捏合（= ctrlKey 的 wheel）无法与普通滚动区分。
      ctrlKey: (ptrMods & 1) !== 0,
      shiftKey: (ptrMods & 2) !== 0,
      altKey: (ptrMods & 4) !== 0,
      metaKey: (ptrMods & 8) !== 0,
    };
    if ("relatedTarget" in o) init.relatedTarget = o.relatedTarget || null;
    // `button: -1` 无法经 MouseEvent 构造器表达：Chromium 把 -1 规范化成 0
    // （实测 `new MouseEvent("x", {button:-1}).button === 0`，而 -2 能原样通过 ——
    // 不是钳位，是对 -1 的特殊处理）。PointerEvent 构造器则保留 -1。
    // 所以 mouse 类事件必须在构造后把 -1 盖回去，否则「移动=左键按下」的坑
    // 只在 pointer 路径修好、mouse 路径依旧（2517518192 恰好走 pointer，
    // 光看它会误以为已经修完）。
    var needsButtonPatch = init.button < 0;
    var ev = null;
    if (isPointer) {
      init.pointerId = 1;
      init.pointerType = "mouse";
      init.isPrimary = true;
      init.width = 1;
      init.height = 1;
      init.pressure = init.buttons ? 0.5 : 0;
      try {
        if (typeof w.PointerEvent === "function") ev = new w.PointerEvent(type, init);
      } catch (_) {
        /* 退回 MouseEvent */
      }
    }
    if (!ev) {
      try {
        if (typeof w.MouseEvent === "function") ev = new w.MouseEvent(type, init);
      } catch (_) {
        /* 忽略 */
      }
    }
    if (ev && needsButtonPatch && ev.button !== init.button) {
      try {
        Object.defineProperty(ev, "button", { configurable: true, get: function () {
          return init.button;
        } });
      } catch (_) {
        /* 只读且不可重定义时保持构造值 */
      }
    }
    return ev;
  }

  function ptrDispatch(node, type, x, y, opts) {
    if (!node || typeof node.dispatchEvent !== "function") return;
    var ev = ptrMakeEvent(type, x, y, opts);
    if (!ev) return;
    try {
      node.dispatchEvent(ev);
    } catch (_) {
      /* 作者处理器抛错不打断后续事件（与官方 CEF 一致：一个坏 listener 不该
         让整条链断掉，否则 leave 发不出去会留下永久 hover/按下态） */
    }
  }

  /** 命中元素变化时补 out/leave + over/enter 四段，顺序与浏览器一致。 */
  function ptrCrossBoundary(prev, next, x, y) {
    if (prev === next) return;
    var ancestor = ptrCommonAncestor(prev, next);
    if (prev) {
      ptrDispatch(prev, "pointerout", x, y, { relatedTarget: next });
      ptrDispatch(prev, "mouseout", x, y, { relatedTarget: next });
      var leaving = ptrChain(prev);
      for (var i = 0; i < leaving.length; i++) {
        if (leaving[i] === ancestor) break;
        // leave 不冒泡：必须逐个发，且 target 就是它自己
        ptrDispatch(leaving[i], "pointerleave", x, y, {
          bubbles: false,
          cancelable: false,
          relatedTarget: next,
        });
        ptrDispatch(leaving[i], "mouseleave", x, y, {
          bubbles: false,
          cancelable: false,
          relatedTarget: next,
        });
      }
    }
    if (next) {
      ptrDispatch(next, "pointerover", x, y, { relatedTarget: prev });
      ptrDispatch(next, "mouseover", x, y, { relatedTarget: prev });
      var entering = [];
      var chain = ptrChain(next);
      for (var j = 0; j < chain.length; j++) {
        if (chain[j] === ancestor) break;
        entering.push(chain[j]);
      }
      // enter 由外向内（祖先先收到），与浏览器一致
      for (var k = entering.length - 1; k >= 0; k--) {
        ptrDispatch(entering[k], "pointerenter", x, y, {
          bubbles: false,
          cancelable: false,
          relatedTarget: prev,
        });
        ptrDispatch(entering[k], "mouseenter", x, y, {
          bubbles: false,
          cancelable: false,
          relatedTarget: prev,
        });
      }
    }
  }

  /**
   * 外部指针注入入口。
   *
   * @param {number} x 相对 iframe 视口左边的 **CSS 像素**（= clientX 空间）
   * @param {number} y 同上，相对上边，Y 朝下
   * @param {number} [buttons] 按键位掩码，bit0 左键。与场景通道同一约定，
   *   当前只消费 bit0（右/中键位保留；桌面右键属于 Finder，不该被壁纸劫持）
   * @param {number} [mods] 修饰键掩码：bit0 ctrl / bit1 shift / bit2 alt / bit3 meta。
   *   省略等于 0（改动前的全 false 行为）
   *
   * 接**像素**而不是归一化坐标：网页壁纸的 iframe 在 cover 露底自适配下可能比舞台大
   * 并带居中偏移（见 web.ts installLetterboxFix），换算需要 iframe 的几何 —— 那是父页
   * 才知道的信息，父页换算完再推进来，shim 不做二次除法。
   *
   * 暂停期间丢弃：官方暂停语义是「冻结渲染进程」，此时派发事件会让作者的动画状态
   * 在冻结中继续推进，恢复时画面跳一下。
   */
  w.__wePushPointer = function (x, y, buttons, mods) {
    if (paused) return;
    var nx = Number(x);
    var ny = Number(y);
    // 非有限值直接丢弃（与场景通道同一约定）：NaN 传进 clientX 会让 elementFromPoint
    // 返回 null、后续 offsetX 全成 NaN，作者的位移积分会一次性污染成 NaN 且不报错。
    if (!isFinite(nx) || !isFinite(ny)) return;
    var mask = Number(buttons) || 0;
    ptrMods = Number(mods) || 0;
    var moved = !ptrHas || nx !== ptrX || ny !== ptrY;
    var maskChanged = mask !== ptrButtons;
    // 位置与按键都没变就什么都不发：宿主按 ~90Hz 推送，静止时重复派发
    // mousemove 会让作者的「有没有在动」判定（1081733658 网格）永远认为在动。
    if (!moved && !maskChanged) return;

    var dx = ptrHas ? nx - ptrX : 0;
    var dy = ptrHas ? ny - ptrY : 0;
    ptrX = nx;
    ptrY = ny;
    ptrHas = true;

    var target = ptrHitTest(nx, ny);
    if (moved) {
      ptrCrossBoundary(ptrTarget, target, nx, ny);
      ptrTarget = target;
      ptrDispatch(target, "pointermove", nx, ny, { movementX: dx, movementY: dy });
      ptrDispatch(target, "mousemove", nx, ny, { movementX: dx, movementY: dy });
    } else {
      ptrTarget = target;
    }

    if (!maskChanged) return;
    var wasDown = (ptrButtons & 1) !== 0;
    var isDown = (mask & 1) !== 0;
    ptrButtons = mask;
    if (isDown === wasDown) return; // 只有高位变化：当前不消费
    if (isDown) {
      ptrDownTarget = target;
      ptrDispatch(target, "pointerdown", nx, ny, { button: 0, detail: 1 });
      ptrDispatch(target, "mousedown", nx, ny, { button: 0, detail: 1 });
      return;
    }
    ptrDispatch(target, "pointerup", nx, ny, { button: 0, detail: 1 });
    ptrDispatch(target, "mouseup", nx, ny, { button: 0, detail: 1 });
    // click 只在 down/up 落在同一元素上时发（否则是拖拽，浏览器也不发）
    if (ptrDownTarget && ptrDownTarget === target) {
      var now = Date.now();
      var isDouble =
        ptrLastClickTarget === target && now - ptrLastClickTime <= PTR_DBLCLICK_MS;
      ptrDispatch(target, "click", nx, ny, { button: 0, detail: isDouble ? 2 : 1 });
      if (isDouble) {
        ptrDispatch(target, "dblclick", nx, ny, { button: 0, detail: 2 });
        ptrLastClickTarget = null;
        ptrLastClickTime = 0;
      } else {
        ptrLastClickTarget = target;
        ptrLastClickTime = now;
      }
    }
    ptrDownTarget = null;
  };

  /**
   * 外部滚轮注入（宿主捕获 scrollWheel / magnify 手势后推入）。
   *
   * @param {number} x 相对 iframe 视口左边的 **CSS 像素**（= clientX 空间）
   * @param {number} y 同上，相对上边，Y 朝下
   * @param {number} dx 横向滚动量，正 = 内容向右（与 DOM deltaX 同向）
   * @param {number} dy 纵向滚动量，正 = 内容向下（与 DOM deltaY 同向，
   *   与 macOS NSEvent.scrollingDeltaY **反向**，取反由宿主负责）
   * @param {number} [mode] deltaMode：0 像素 / 1 行 / 2 页。触摸板与 Magic Mouse 恒为 0
   * @param {number} [mods] 修饰键掩码，bit0 ctrl。**触摸板双指捏合 = ctrl + 滚轮**
   *
   * ---- 为什么必须补发旧式 `mousewheel`（语料决定，漏了命中率为 0）----
   *
   * 本机 52 张网页壁纸里真正消费滚轮的三处**全都不听现代 `wheel`**：
   *   - 3406740580 pano2vr（唯一作者设计内的滚轮交互，滚轮改全景 FOV）：
   *     `addEventListener("mousewheel")` + `("DOMMouseScroll")`，handler 取
   *     `a.detail ? -1*a.detail : a.wheelDelta/40`；
   *   - 2179153203 ge1doot：`onmousewheel` 里 `-event.wheelDelta * .25`；
   *   - 2517518192 GameMaker 运行时：`canvas.onmousewheel` + DOMMouseScroll。
   * 只听 `wheel` 的是 OrbitControls（1808443523）与 react-lrc（2905017768）。
   * 所以两路都得发，只发任意一路都有真实壁纸完全无反应。
   *
   * ---- 为什么**不**发 `DOMMouseScroll`（否则滚动量翻倍）----
   *
   * 上面三处旧式消费方**每一处都同时注册了 `mousewheel` 和 `DOMMouseScroll`**，
   * 而它们的 handler 是同一个函数。两个都发 = 同一次滚动被处理两遍，pano2vr 的
   * FOV 一次跳两格，且看起来只是「滚轮太灵敏」，不像 bug。真实浏览器也从不同时发
   * 这两个（Chromium 只发 wheel + mousewheel）。且全语料没有任何一张只听
   * DOMMouseScroll —— 它没有独占消费方，发它纯是负收益。
   *
   * 暂停期间丢弃，与 __wePushPointer 一致。
   */
  w.__wePushWheel = function (x, y, dx, dy, mode, mods) {
    if (paused) return;
    // 先转数再判有限，**不要**写 `Number(dx) || 0`：那会把 NaN 静默变成 0，
    // 于是「非有限值丢弃」这条约定形同虚设（NaN 的 dy 会被当成 0 放过去，
    // 再与合法的 dx 一起派发出一个半污染的事件）。
    var ndx = Number(dx);
    var ndy = Number(dy);
    // 非有限值丢弃（与指针通道同一约定）：NaN 的 deltaY 会污染作者的缩放累加器，
    // 之后无论怎么滚都恢复不了，且没有任何报错。
    if (!isFinite(ndx) || !isFinite(ndy)) return;
    // 两个方向都是 0 就什么都不发：宿主在惯性滚动尾声会推一串 0，
    // 空事件会让作者的「有没有在滚」判定一直为真。
    if (ndx === 0 && ndy === 0) return;
    var dmode = Number(mode) || 0;
    ptrMods = Number(mods) || 0;

    // 位置：滚轮事件本身不带位置，用最后已知的指针位置。没收到过指针时取视口中心
    // 而不是 (0,0) —— OrbitControls 一族按事件坐标定缩放锚点，落在左上角会让画面
    // 一边缩放一边往角上跑。
    var px = ptrX;
    var py = ptrY;
    if (!ptrHas) {
      px = ptrViewportW() / 2;
      py = ptrViewportH() / 2;
    }
    var nx = Number(x);
    var ny = Number(y);
    if (isFinite(nx) && isFinite(ny)) {
      px = nx;
      py = ny;
      ptrX = nx;
      ptrY = ny;
      ptrHas = true;
    }

    var target = ptrHitTest(px, py);
    // 命中元素变了要先补边界链：作者可能靠 mouseenter 才开始接滚轮
    // （与 __wePushPointer 同一理由），且 pano2vr 的 handler 开头就 `this.zc(a.target)`
    // 校验命中是不是自己的容器。
    if (target !== ptrTarget) {
      ptrCrossBoundary(ptrTarget, target, px, py);
      ptrTarget = target;
    }

    // (1) 现代 `wheel`。**必须 cancelable**：pano2vr 与 ge1doot 都在 handler 里调
    // preventDefault()，不可取消时 Chromium 会在控制台刷 Unable to preventDefault
    // 且作者的 `return false` 分支语义漂移。
    ptrDispatchWheel(target, px, py, ndx, ndy, dmode);

    // (2) 旧式 `mousewheel`（Chromium 的 legacy alias，与真实浏览器同款组合）。
    // wheelDelta 与 deltaY **反号**：一格标准滚动在 Chromium 里是 deltaY=+100、
    // wheelDelta=-120，故系数 1.2。pano2vr 的 `wheelDelta/40` 得 -3 → 缩小，
    // 与真实浏览器里滚下缩小一致；符号搞反会让所有旧式壁纸的滚轮方向整体反过来。
    var pxPerUnit = dmode === 1 ? WHEEL_LINE_PX : dmode === 2 ? ptrViewportH() || 800 : 1;
    var legacyY = -ndy * pxPerUnit * 1.2;
    var legacyX = -ndx * pxPerUnit * 1.2;
    var ev = ptrMakeEvent("mousewheel", px, py, { button: -1 });
    if (ev) {
      ptrDefine(ev, "wheelDelta", legacyY);
      ptrDefine(ev, "wheelDeltaY", legacyY);
      ptrDefine(ev, "wheelDeltaX", legacyX);
      // detail 恒为 0：旧式 Firefox 那套 `a.detail ? -1*a.detail : a.wheelDelta/40`
      // 的三元判断里，detail 非 0 会抢在 wheelDelta 之前被采用（且量级完全不同）。
      ptrDefine(ev, "detail", 0);
      try {
        target && target.dispatchEvent && target.dispatchEvent(ev);
      } catch (_) {
        /* 作者 handler 抛错不打断（与 ptrDispatch 同一理由） */
      }
    }
  };

  /** 一格「行」滚动折算的像素数，与 Chromium 的 kDefaultLineHeight 量级一致。 */
  var WHEEL_LINE_PX = 40;

  function ptrViewportW() {
    try {
      return Number(w.innerWidth) || Number(w.document.documentElement.clientWidth) || 0;
    } catch (_) {
      return 0;
    }
  }

  function ptrViewportH() {
    try {
      return Number(w.innerHeight) || Number(w.document.documentElement.clientHeight) || 0;
    } catch (_) {
      return 0;
    }
  }

  /** 在合成事件上补一个只读字段（构造器不认识的 legacy 字段只能这么给）。 */
  function ptrDefine(ev, key, value) {
    try {
      Object.defineProperty(ev, key, {
        configurable: true,
        get: function () {
          return value;
        },
      });
    } catch (_) {
      try {
        ev[key] = value;
      } catch (__) {
        /* 只读且不可重定义时放弃该字段 */
      }
    }
  }

  /**
   * 派发现代 `wheel`。优先真 `WheelEvent`（作者读 deltaMode / deltaZ 时才对）；
   * 环境没有时退回 MouseEvent 再补字段 —— 事件名照旧，`addEventListener('wheel')`
   * 仍然收到。
   */
  function ptrDispatchWheel(target, x, y, dx, dy, mode) {
    if (!target || typeof target.dispatchEvent !== "function") return;
    var init = {
      bubbles: true,
      cancelable: true,
      composed: true,
      view: w,
      detail: 0,
      clientX: x,
      clientY: y,
      screenX: x + (Number(w.screenX) || 0),
      screenY: y + (Number(w.screenY) || 0),
      // 滚轮不是按键状态变化，button 取 -1 哨兵（与移动类事件同一理由，
      // 填 0 会让 GameMaker 一族误认为左键按着）
      button: -1,
      buttons: ptrButtons,
      ctrlKey: (ptrMods & 1) !== 0,
      shiftKey: (ptrMods & 2) !== 0,
      altKey: (ptrMods & 4) !== 0,
      metaKey: (ptrMods & 8) !== 0,
      deltaX: dx,
      deltaY: dy,
      deltaZ: 0,
      deltaMode: mode,
    };
    var ev = null;
    try {
      if (typeof w.WheelEvent === "function") ev = new w.WheelEvent("wheel", init);
    } catch (_) {
      /* 退回 MouseEvent */
    }
    if (!ev) {
      try {
        if (typeof w.MouseEvent === "function") ev = new w.MouseEvent("wheel", init);
      } catch (_) {
        /* 忽略 */
      }
    }
    if (!ev) return;
    // MouseEvent 退路上 delta* 不会被构造器采纳，必须补；WheelEvent 路径下
    // 读到的值与 init 相同，条件不成立，补也不会发生。
    if (ev.deltaX !== dx) ptrDefine(ev, "deltaX", dx);
    if (ev.deltaY !== dy) ptrDefine(ev, "deltaY", dy);
    if (ev.deltaMode !== mode) ptrDefine(ev, "deltaMode", mode);
    if (ev.button !== -1) ptrDefine(ev, "button", -1);
    try {
      target.dispatchEvent(ev);
    } catch (_) {
      /* 作者 handler 抛错不打断后续旧式事件 */
    }
  }

  /**
   * 指针离开本窗口（鼠标去了别的显示器）。
   *
   * 与场景通道不同，这里**必须把 out/leave 链发出去**：场景侧只是清一个状态位，
   * 而网页作者的 hover 态是自己记的，不发 leave 就永久卡在「鼠标还在上面」
   * （1081733658 网格会一直跑、1748506393 的 `pointers[0].down` 一直为 true）。
   * 按下态也要补一次 up，否则拖拽逻辑永远不结束。
   */
  w.__wePointerLeave = function () {
    if ((ptrButtons & 1) !== 0 && ptrTarget) {
      ptrDispatch(ptrTarget, "pointerup", ptrX, ptrY, { button: 0, buttons: 0, detail: 1 });
      ptrDispatch(ptrTarget, "mouseup", ptrX, ptrY, { button: 0, buttons: 0, detail: 1 });
    }
    ptrButtons = 0;
    ptrDownTarget = null;
    if (ptrTarget) {
      ptrCrossBoundary(ptrTarget, null, ptrX, ptrY);
      ptrTarget = null;
    }
    // 位置（ptrX/ptrY）与 ptrHas 保留：下次进来时 movement 才是真实位移，
    // 而不是从 (0,0) 跳过来的一个巨大假 delta。
  };

  // —— rAF 节流（带 __weThrottled，避免父页 injectGpuThrottle 双层减半）——

  function installRafThrottle() {
    var throttled = function (cb) {
      if (typeof cb !== "function") return 0;
      if (paused) {
        var idHold = ++rafCounter;
        rafMap[idHold] = { kind: "hold", cb: cb };
        return idHold;
      }
      var limit = fps >= 60 ? 0 : 1000 / fps;
      if (limit <= 0) {
        var idNative = origRaf(function (now) {
          delete rafMap[idNative];
          try {
            cb(now);
          } catch (_) {
            /* 忽略 */
          }
          try {
            w.parent.postMessage({ op: "we-frame", t: now }, "*");
          } catch (_) {
            /* 忽略 */
          }
        });
        rafMap[idNative] = { kind: "native", id: idNative };
        return idNative;
      }
      var id = ++rafCounter;
      var to = w.setTimeout(function () {
        delete rafMap[id];
        origRaf(function (now) {
          try {
            cb(now);
          } catch (_) {
            /* 忽略 */
          }
          try {
            w.parent.postMessage({ op: "we-frame", t: now }, "*");
          } catch (_) {
            /* 忽略 */
          }
        });
      }, limit);
      rafMap[id] = { kind: "timeout", to: to };
      return id;
    };
    throttled.__weThrottled = true;
    w.requestAnimationFrame = throttled;
    w.cancelAnimationFrame = function (id) {
      var ent = rafMap[id];
      if (!ent) {
        try {
          origCaf(id);
        } catch (_) {
          /* 忽略 */
        }
        return;
      }
      delete rafMap[id];
      if (ent.kind === "timeout") w.clearTimeout(ent.to);
      else if (ent.kind === "native") origCaf(ent.id);
    };
  }

  installRafThrottle();
})(window);

