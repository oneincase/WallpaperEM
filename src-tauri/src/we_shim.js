/*!
 * Wallpaper Engine 网页壁纸兼容 shim（注入到库内壁纸 HTML 的 <head> 开头，
 * 先于壁纸自身脚本执行）。对齐 WE 桌面端行为：
 *
 * 1. window.wallpaperPropertyListener —— 用 defineProperty 捕获壁纸的赋值，
 *    赋值后立即回调 applyGeneralProperties({fps}) 与 applyUserProperties(props)；
 *    属性热更新时（__weApplyProps）对所有已注册 listener 再次回调。
 * 2. window.wallpaperRegisterAudioListener(cb) —— 64 段对数频谱，每帧推送
 *    128 长度数组（前 64 段低→高，后 64 段镜像，与 WE 数据格式一致）。
 *    双数据源取最大值融合：
 *    - 系统全局音频（开关开启时）：Rust 端 ScreenCaptureKit loopback → FFT，
 *      经内容服务器 /audio-stream SSE 推送，shim 内 EventSource 订阅
 *    - 壁纸自身音频：`<audio>` 元素经 WebAudio AnalyserNode 分析
 *    - 壁纸 WebAudio 合成音频：包装 AudioContext 构造器，连到 destination
 *      的节点同步接入 tap 分析器
 * 3. rAF 帧率节流 —— 等价渲染器的 injectGpuThrottle，但注入时机更早，
 *    壁纸脚本拿到的 requestAnimationFrame 已是节流版。
 *
 * 渲染器通过 __weSetFps / __weSetPaused / __weSetVolume / __weApplyProps 控制。
 * 注意：本文件会被内联进 script 标签，源码中不得出现 script 结束标签的字面量
 * （包括注释里），否则 HTML 解析器会在此提前闭合标签。
 */
(function () {
  "use strict";
  var boot = window.__WE_BOOT || {};
  var BANDS = 64;

  // ---------- 状态 ----------
  var userProps = boot.props || {};
  var fps = boot.fps || 60;
  var paused = false;
  var propListeners = [];
  var audioCbs = [];
  var curListener = null;

  // ---------- 属性 listener 捕获 ----------
  function dispatchTo(l, includeGeneral) {
    if (!l || typeof l !== "object") return;
    if (includeGeneral !== false) {
      try {
        if (typeof l.applyGeneralProperties === "function") {
          l.applyGeneralProperties({ fps: fps });
        }
      } catch (e) {}
    }
    try {
      if (typeof l.applyUserProperties === "function") {
        l.applyUserProperties(userProps);
      }
    } catch (e) {}
  }

  try {
    Object.defineProperty(window, "wallpaperPropertyListener", {
      configurable: true,
      get: function () {
        return curListener;
      },
      set: function (v) {
        // 重复赋值视为替换（避免同一段脚本被热载时 listener 越积越多）
        var i = propListeners.indexOf(curListener);
        if (i >= 0) propListeners.splice(i, 1);
        curListener = v;
        if (v && typeof v === "object") {
          propListeners.push(v);
          dispatchTo(v, true);
        }
      },
    });
  } catch (e) {
    // defineProperty 不可用（极老内核）：退化为 load 后一次性补发
    window.addEventListener("load", function () {
      dispatchTo(window.wallpaperPropertyListener, true);
    });
  }

  // ---------- 音频 ----------
  // 优先用原生 AudioContext（在包装前保存引用），自身分析不经过 tap，避免自环
  var RawAudioContext = window.AudioContext || window.webkitAudioContext;
  var actx = null;
  var allAnalysers = []; // [{analyser, buf}]：媒体元素分析器 + WebAudio tap 分析器
  var mediaAnalyser = null;
  var bandEdges = null;
  var hookedSet = new WeakSet();
  var hookedEls = [];
  var volumeCur = null; // null = 不干预（沿用壁纸自身音量）
  var spec = new Float32Array(BANDS); // 本地分析（媒体元素 + 合成音频 tap）
  var extSpec = new Float32Array(BANDS); // 系统音频（SSE 外部帧）
  var externalUntil = 0;
  var lastResumeTry = 0;

  function makeAnalyser(ctx) {
    var an = ctx.createAnalyser();
    an.fftSize = 2048;
    an.smoothingTimeConstant = 0.8;
    return { analyser: an, buf: new Uint8Array(an.frequencyBinCount) };
  }

  function ensureCtx() {
    if (actx) return true;
    if (!RawAudioContext) return false;
    try {
      actx = new RawAudioContext();
      actx.__weInternal = true; // 自身上下文不参与 WebAudio tap
      var m = makeAnalyser(actx);
      mediaAnalyser = m.analyser;
      allAnalysers.push(m);
      // 分析器必须回连 destination（媒元素源接管了出声通路）
      mediaAnalyser.connect(actx.destination);
      bandEdges = buildEdges(m.analyser.frequencyBinCount, actx.sampleRate);
      return true;
    } catch (e) {
      actx = null;
      mediaAnalyser = null;
      return false;
    }
  }

  // 对数分箱：30Hz~min(16kHz, Nyquist)，边沿保证单调递增
  function buildEdges(binCount, sampleRate) {
    var fmin = 30;
    var fmax = Math.min(16000, sampleRate / 2);
    var edges = new Uint32Array(BANDS + 1);
    for (var i = 0; i <= BANDS; i++) {
      var f = fmin * Math.pow(fmax / fmin, i / BANDS);
      edges[i] = Math.min(binCount - 1, Math.max(0, Math.round((f / (sampleRate / 2)) * binCount)));
    }
    for (var j = 1; j <= BANDS; j++) {
      if (edges[j] <= edges[j - 1]) edges[j] = edges[j - 1] + 1;
    }
    return edges;
  }

  function hookMedia(el) {
    if (!el || hookedSet.has(el)) return;
    // 仅 hook <audio>：<video> 建媒元素源有真实风险——AudioContext 被自动播放
    // 策略挂起时，其音轨经挂起上下文会静音；纯画面视频 WebGL 纹理不受影响。
    if (!(el instanceof HTMLAudioElement)) return;
    hookedSet.add(el);
    hookedEls.push(el);
    if (hookedEls.length > 64) hookedEls = hookedEls.filter(function (e) { return e.isConnected; });
    if (volumeCur !== null) {
      try { el.volume = volumeCur; } catch (e) {}
    }
    if (!ensureCtx()) return;
    try {
      // MediaElementSource 会接管元素出声通路 → 分析器已在 ensureCtx 回连 destination
      actx.createMediaElementSource(el).connect(mediaAnalyser);
      if (actx.state === "suspended") actx.resume().catch(function () {});
    } catch (e) {
      /* 已连接过/不支持则忽略 */
    }
  }

  // 播放事件捕获：覆盖 autoplay 属性（内部调用不经过 JS 层 play()）
  document.addEventListener(
    "play",
    function (e) {
      var t = e.target;
      if (t && t.tagName === "AUDIO") hookMedia(t);
    },
    true
  );
  // play() 调用捕获：覆盖脱离 DOM 的 new Audio()（事件冒泡不到 document）
  try {
    var origPlay = HTMLMediaElement.prototype.play;
    HTMLMediaElement.prototype.play = function () {
      try { hookMedia(this); } catch (e) {}
      return origPlay.apply(this, arguments);
    };
  } catch (e) {}

  window.wallpaperRegisterAudioListener = function (cb) {
    if (typeof cb === "function") audioCbs.push(cb);
  };

  // 系统音频外部帧写入（SSE onmessage / 调试注入用），250ms 内参与融合
  window.__weAudio = {
    push: function (data) {
      try {
        var n = data ? data.length : 0;
        for (var i = 0; i < BANDS; i++) extSpec[i] = i < n ? Number(data[i]) || 0 : 0;
        externalUntil = performance.now() + 250;
      } catch (e) {}
    },
  };

  // ---------- 系统音频（SSE 外部帧）：Rust ScreenCaptureKit → /audio-stream ----------
  // 与本地分析（壁纸自身音频）取最大值融合：系统音乐驱动 extSpec，壁纸自播驱动 spec
  if (boot.systemAudio && boot.token && /^http:\/\/127\.0\.0\.1:\d+$/.test(location.origin)) {
    try {
      var es = new EventSource(location.origin + "/audio-stream/" + boot.token);
      es.onmessage = function (ev) {
        window.__weAudio.push(JSON.parse(ev.data));
      };
    } catch (e) {}
  }

  // ---------- WebAudio 合成音频 tap：包装 AudioContext，连到 destination 的节点同步接入 tap ----------
  var ctxTaps = new WeakMap(); // ctx → {analyser, buf} | null
  function tapFor(ctx) {
    var t = ctxTaps.get(ctx);
    if (t !== undefined) return t;
    t = null;
    try {
      var an = ctx.createAnalyser();
      an.fftSize = 2048;
      an.smoothingTimeConstant = 0.8;
      t = { analyser: an, buf: new Uint8Array(an.frequencyBinCount) };
      allAnalysers.push(t);
    } catch (e) {
      t = null;
    }
    ctxTaps.set(ctx, t);
    return t;
  }
  if (RawAudioContext && typeof AudioNode !== "undefined") {
    // 包装构造器（原型直挂保持 instanceof；构造器返回对象覆盖 this）
    var PatchedAudioContext = function () {
      var ctx = Reflect.construct(RawAudioContext, arguments);
      try {
        if (!ctx.__weInternal) tapFor(ctx);
      } catch (e) {}
      return ctx;
    };
    PatchedAudioContext.prototype = RawAudioContext.prototype;
    try { window.AudioContext = PatchedAudioContext; } catch (e) {}
    if (window.webkitAudioContext === RawAudioContext) {
      try { window.webkitAudioContext = PatchedAudioContext; } catch (e) {}
    }
    // destination 连接旁路 tap：节点连到 destination 时同步连一份到该上下文的 tap 分析器
    var origConnect = AudioNode.prototype.connect;
    AudioNode.prototype.connect = function (dst) {
      var r = origConnect.apply(this, arguments);
      try {
        if (dst instanceof AudioDestinationNode) {
          var ctx = this.context;
          if (ctx && !ctx.__weInternal) {
            var tap = tapFor(ctx);
            if (tap) origConnect.call(this, tap.analyser);
          }
        }
      } catch (e) {}
      return r;
    };
  }

  var frame = new Float32Array(BANDS * 2);

  function computeSpectrum() {
    for (var z = 0; z < BANDS; z++) spec[z] = 0;
    if (allAnalysers.length === 0) return;
    // 懒初始化分箱：壁纸可能只用合成音频（从未触发媒体元素路径的 ensureCtx）
    if (!bandEdges) {
      var first = allAnalysers[0].analyser;
      bandEdges = buildEdges(first.frequencyBinCount, first.context.sampleRate);
    }
    for (var a = 0; a < allAnalysers.length; a++) {
      var an = allAnalysers[a].analyser;
      var buf = allAnalysers[a].buf;
      try {
        if (an.context.state !== "running") continue;
      } catch (e) { continue; }
      an.getByteFrequencyData(buf);
      for (var i = 0; i < BANDS; i++) {
        var lo = bandEdges[i];
        var hi = Math.min(bandEdges[i + 1], buf.length);
        var m = 0;
        for (var b = lo; b < hi; b++) {
          if (buf[b] > m) m = buf[b];
        }
        var v = m / 255;
        if (v > spec[i]) spec[i] = v;
      }
    }
  }

  function broadcastFrame(now) {
    if (paused || audioCbs.length === 0) return;
    // 本地始终分析（壁纸自播音乐）；系统音频（排除本进程）覆盖外部声音，
    // 双路逐段取最大，任一来源有能量即可视化
    computeSpectrum();
    var extFresh = now < externalUntil;
    for (var i = 0; i < BANDS; i++) {
      var vi = spec[i];
      var mir = BANDS - 1 - i;
      var vm = spec[mir];
      if (extFresh) {
        if (extSpec[i] > vi) vi = extSpec[i];
        if (extSpec[mir] > vm) vm = extSpec[mir];
      }
      frame[i] = vi;
      frame[BANDS + i] = vm; // 镜像：WE 消费方常用后半段画对称频谱
    }
    for (var c = 0; c < audioCbs.length; c++) {
      try { audioCbs[c](frame); } catch (e) {}
    }
  }

  // ---------- rAF 节流（fps < 60 时生效；先于壁纸脚本安装） ----------
  var rafNative = window.requestAnimationFrame.bind(window);
  var rafId = 0;
  var rafTimers = new Map();

  function throttledRaf(cb) {
    var id = ++rafId;
    var interval = 1000 / fps;
    var to = window.setTimeout(function () {
      rafTimers.delete(id);
      rafNative(function (now) {
        try { cb(now); } catch (e) {}
      });
    }, interval);
    rafTimers.set(id, to);
    return id;
  }

  function installRafThrottle() {
    if (fps >= 60) return; // 60/120 已是原生帧率或高刷上限，不节流
    window.requestAnimationFrame = throttledRaf;
    window.requestAnimationFrame.__weThrottled = true; // 渲染器兜底注入据此跳过
    window.cancelAnimationFrame = function (id) {
      var to = rafTimers.get(id);
      if (to !== undefined) {
        window.clearTimeout(to);
        rafTimers.delete(id);
      }
    };
  }
  installRafThrottle();

  // ---------- 主循环：音频频谱推送 + AudioContext 恢复兜底 ----------
  (function loop(now) {
    rafNative(loop);
    broadcastFrame(now || 0);
    // 自动播放策略下 AudioContext 可能停在 suspended：有音频消费方时定期重试
    if (actx && actx.state === "suspended" && audioCbs.length > 0 && (now || 0) - lastResumeTry > 2000) {
      lastResumeTry = now || 0;
      actx.resume().catch(function () {});
    }
  })(0);

  // ---------- 渲染器控制接口 ----------
  window.__weSetFps = function (v) {
    fps = v || 60;
    installRafThrottle();
    for (var i = 0; i < propListeners.length; i++) dispatchTo(propListeners[i], true);
  };

  window.__weSetPaused = function (p) {
    paused = !!p;
    for (var i = 0; i < hookedEls.length; i++) {
      var el = hookedEls[i];
      try {
        if (paused) {
          if (!el.paused) {
            el.__weResume = true;
            el.pause();
          }
        } else if (el.__weResume) {
          el.__weResume = false;
          el.play().catch(function () {});
        }
      } catch (e) {}
    }
  };

  window.__weSetVolume = function (v) {
    volumeCur = v;
    for (var i = 0; i < hookedEls.length; i++) {
      try { hookedEls[i].volume = v; } catch (e) {}
    }
  };

  window.__weApplyProps = function (props) {
    if (props && typeof props === "object") {
      var keys = Object.keys(props);
      for (var i = 0; i < keys.length; i++) userProps[keys[i]] = props[keys[i]];
    }
    for (var j = 0; j < propListeners.length; j++) dispatchTo(propListeners[j], false);
  };
})();
