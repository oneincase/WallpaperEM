// WallpaperEM 的 Plasma 壁纸插件（plasmashell 进程内的**显示端**）。
//
// 定位与理由（为什么必须是插件）：
//   外部窗口永远排不到 plasmashell 桌面视图之下，而桌面图标就画在那个窗口里，
//   所以「动态壁纸 + 桌面图标同时可见」只有「和图标同属一个 containment」这一条路。
//   详见 WallpaperEM 仓的 docs/kde-adaptation-plan.md §2.5/§4.1 与真机报告。
//
// 引擎侧契约（完整版见 docs/engine-contract.md）：
//   * 引擎把最新帧写成图片，**先写 <path>.tmp 再 rename 覆盖**（原子替换）；
//     原地覆写会让插件读到半张图（撕裂/花屏）。
//   * 引擎每帧再把自增整数写进 SeqPath（同样原子替换）；插件靠它决定
//     「要不要重新解码」和「引擎是不是停了」。
//   * 插件只读，不写、不删、不改名；帧文件缺失/不可读时不报错，只显示兜底。
//   * 插件崩 = plasmashell 崩 = 整个桌面崩，所以这里不做阻塞 IO、不抛异常。
//
// 为什么带 stub：`wallpaper` 是 plasmashell 注入的上下文属性，用 `qmlscene`
// 单跑本文件时不存在。分支之后本文件可独立渲染（scripts/verify.sh 靠它做
// 「引擎写帧 → 插件显示」的端到端验证）。桩只能用**小写**属性名——
// QML 不允许自定义属性首字母大写，配置键的大小写只存在于 Plasma 注入的 QObject 上。
import QtQuick 2.15
import QtQuick.Window 2.15

Item {
    id: root

    // ================= 配置 =================

    QtObject {
        id: stubConfig
        property string framePath: "/dev/shm/wallpaperem/frame.ppm"
        property string seqPath: "/dev/shm/wallpaperem/frame.seq"
        property bool useSeqFile: true
        property bool perScreen: false
        property int refreshMs: 40
        property string fallbackColor: "#101014"
        property string fallbackImage: ""
        property int fillMode: 1
        property int stallMs: 3000
        property bool showStatusText: true
    }

    readonly property var cfg: (typeof wallpaper !== "undefined"
                                && wallpaper.configuration !== undefined
                                && wallpaper.configuration !== null)
                               ? wallpaper.configuration
                               : (stubOverride.status === Loader.Ready && stubOverride.item
                                  ? stubOverride.item : stubConfig)

    // 独立渲染（qmlscene/预览）时的桩配置覆盖：同目录存在 stub-override.qml 就用它。
    // 生产环境（plasmashell 里）active=false，不加载任何额外文件。
    // scripts/verify.sh 靠这个在不改生产文件的前提下测多屏/静态模式等配置分支。
    Loader {
        id: stubOverride
        active: typeof wallpaper === "undefined"
        source: "stub-override.qml"
    }

    // 统一取值。两种大小写都认：
    //   * plasmashell 注入的配置对象 → 属性名就是 main.xml 里的 <entry name>（FramePath…）；
    //   * 独立渲染时的桩（stubConfig / stub-override.qml）→ QML 不允许属性首字母大写，
    //     只能用 framePath…，所以这里回退查一次小写名。
    function c(key, dflt) {
        if (!cfg) return dflt
        var v = cfg[key]
        if (v === undefined || v === null) {
            var lower = key.charAt(0).toLowerCase() + key.slice(1)
            v = cfg[lower]
        }
        return (v === undefined || v === null) ? dflt : v
    }

    // 本实例所在屏幕名（多屏时每个 containment 一个实例）。
    // 非字母数字换成下划线，保证能安全拼进文件名。
    readonly property string screenName: {
        var w = Window.window
        var n = (w && w.screen && w.screen.name) ? String(w.screen.name) : "default"
        return n.replace(/[^A-Za-z0-9_.-]/g, "_")
    }

    function withScreen(p) {
        if (!p) return ""
        var per = Boolean(c("PerScreen", stubConfig.perScreen))
        return per ? p.replace(/%SCREEN%/g, screenName) : p
    }

    readonly property string framePath: withScreen(String(c("FramePath", stubConfig.framePath)))
    readonly property string seqPath: withScreen(String(c("SeqPath", stubConfig.seqPath)))
    readonly property bool useSeq: Boolean(c("UseSeqFile", stubConfig.useSeqFile)) && seqPath !== ""
    readonly property int refreshMs: Math.max(0, Number(c("RefreshMs", stubConfig.refreshMs)))
    readonly property int fillMode: Number(c("FillMode", stubConfig.fillMode))
    readonly property string fallbackColorStr: {
        var s = String(c("FallbackColor", stubConfig.fallbackColor))
        return s === "" ? "#101014" : s
    }
    readonly property color fallbackColor: fallbackColorStr
    readonly property string fallbackImage: String(c("FallbackImage", stubConfig.fallbackImage))
    readonly property int stallMs: Math.max(500, Number(c("StallMs", stubConfig.stallMs)))
    readonly property bool showStatusText: Boolean(c("ShowStatusText", stubConfig.showStatusText))

    // ================= 状态（供状态文字与调试用） =================

    property bool frameReady: false          // 至少成功显示过一帧
    property bool engineStalled: false       // 序号长时间不变
    property int decodeFailures: 0
    property var lastSeq: ""
    property var lastSeqAt: 0                // Date.now()，最后一次序号变化
    property bool seqUsable: true            // 序号文件读不到时自动退回逐帧解码
    property int seqFailures: 0

    // 引擎当前是否在出帧：
    //   * refreshMs=0（静态、只读一次）→ 不做「停止」判断，能显示就算活着；
    //   * 序号文件可用 → 只看「最近有没有变化」（超过 stallMs 没变 = 停了）；
    //   * 序号文件不可用（引擎没实现/读不到）→ 能解码出帧且没连错就算活着。
    readonly property bool engineAlive: {
        if (refreshMs === 0) return frameReady
        if (!useSeq || !seqUsable || lastSeqAt === 0) return frameReady && decodeFailures === 0
        return !engineStalled
    }

    Component.onCompleted: {
        console.info("WallpaperEM plugin: frames=" + framePath
                     + " seq=" + (useSeq ? seqPath : "(off)")
                     + " refresh=" + refreshMs + "ms perScreen=" + Boolean(c("PerScreen", stubConfig.perScreen))
                     + " screen=" + screenName)
        hardReload()
        if (useSeq) pollSeq()
    }

    // ================= 画面 =================

    Rectangle {
        id: backdrop
        anchors.fill: parent
        color: root.fallbackColor
    }

    // 帧不可用时（可选的）静态兜底图，比纯色好看
    Image {
        id: fallbackImageItem
        anchors.fill: parent
        source: root.fallbackImage === "" ? "" : "file://" + root.fallbackImage
        fillMode: root.fillMode === 0 ? Image.Stretch
                : root.fillMode === 2 ? Image.PreserveAspectFit
                                      : Image.PreserveAspectCrop
        asynchronous: true
        visible: !root.frameReady && status === Image.Ready
    }

    // 双缓冲：新帧解码完成前，旧帧继续画 —— 否则每次刷新都会闪一下黑
    Image {
        id: bufferA
        anchors.fill: parent
        cache: false
        asynchronous: true
        smooth: true
        visible: false
        fillMode: root.imageFillMode
        onStatusChanged: root.onBufferStatus(bufferA)
    }
    Image {
        id: bufferB
        anchors.fill: parent
        cache: false
        asynchronous: true
        smooth: true
        visible: false
        fillMode: root.imageFillMode
        onStatusChanged: root.onBufferStatus(bufferB)
    }

    readonly property int imageFillMode: fillMode === 0 ? Image.Stretch
                                        : fillMode === 2 ? Image.PreserveAspectFit
                                                         : Image.PreserveAspectCrop

    property var frontBuffer: null
    property var pendingBuffer: null
    property int tick: 0

    function onBufferStatus(buf) {
        if (buf !== pendingBuffer) return
        if (buf.status === Image.Ready) {
            if (frontBuffer && frontBuffer !== buf) frontBuffer.visible = false
            buf.visible = true
            frontBuffer = buf
            pendingBuffer = null
            frameReady = true
            engineStalled = false
            decodeFailures = 0
        } else if (buf.status === Image.Error) {
            pendingBuffer = null
            decodeFailures += 1
            // 偶尔失败不必刷日志（引擎重启/换帧的瞬间很正常）
            if (decodeFailures === 1 || decodeFailures % 50 === 0)
                console.warn("WallpaperEM plugin: 帧读取失败 (" + decodeFailures + " 次): " + buf.source)
        }
    }

    // 强制解一帧（序号变了、或没启用序号时每个 tick 调一次）
    function hardReload() {
        if (framePath === "" || refreshMs === 0 && tick > 0) return
        var buf = (frontBuffer === bufferA) ? bufferB : bufferA
        if (pendingBuffer !== null && pendingBuffer.status === Image.Loading) return  // 上一帧还在解，丢掉这次
        pendingBuffer = buf
        buf.source = "file://" + framePath + "?v=" + tick
    }

    // 读序号文件：变了才解码；连续读不到就永久退回逐帧解码
    function pollSeq() {
        if (!useSeq || !seqUsable || pendingSeq) return
        pendingSeq = true
        var xhr = new XMLHttpRequest()
        xhr.onreadystatechange = function () {
            if (xhr.readyState !== XMLHttpRequest.DONE) return
            pendingSeq = false
            var txt = String(xhr.responseText || "").trim()
            if (txt !== "") {
                seqFailures = 0
                var now = Date.now()
                if (txt !== lastSeq) {
                    lastSeq = txt
                    lastSeqAt = now
                    engineStalled = false
                    tick += 1
                    hardReload()
                } else if (lastSeqAt > 0 && now - lastSeqAt > stallMs) {
                    engineStalled = true
                }
            } else {
                seqFailures += 1
                // 序号文件还不存在（引擎没起来）是常态；连续失败才放弃这个优化
                if (seqFailures >= 3) {
                    seqUsable = false
                    console.info("WallpaperEM plugin: 序号文件不可用，退回逐帧解码")
                    hardReload()
                }
            }
        }
        // 缓存穿透：同一个路径每次都要读到最新内容
        xhr.open("GET", "file://" + seqPath + "?v=" + Date.now(), true)
        xhr.send()
    }
    property bool pendingSeq: false

    Timer {
        id: loop
        // refreshMs=0 → 只读一次；没有帧路径时不空转
        running: framePath !== "" && refreshMs > 0
        interval: Math.max(16, refreshMs)
        repeat: true
        onTriggered: {
            if (useSeq && seqUsable) {
                pollSeq()
                // 序号文件在、且没变化 → 检查引擎是否已经静默超时
                if (lastSeqAt > 0 && Date.now() - lastSeqAt > stallMs) engineStalled = true
            } else {
                tick += 1
                hardReload()
            }
        }
    }

    // 换屏/改缩放后几何变化，强制重新解码一张（避免拉伸模糊残留）
    onWidthChanged: if (frameReady) { tick += 1; hardReload() }
    onHeightChanged: if (frameReady) { tick += 1; hardReload() }

    // ================= 状态文字 =================

    Text {
        anchors.centerIn: parent
        visible: root.showStatusText && !root.frameReady
        color: "#9a9aa2"
        font.pixelSize: 14
        horizontalAlignment: Text.AlignHCenter
        text: qsTr("WallpaperEM 引擎未运行\n启动引擎后本壁纸会自动显示画面")
    }

    Text {
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom: parent.bottom
        anchors.bottomMargin: 24
        visible: root.showStatusText && root.frameReady && !root.engineAlive
        color: "#c8a36a"
        font.pixelSize: 13
        text: qsTr("WallpaperEM 引擎已停止输出（显示最后一帧）")
    }
}
