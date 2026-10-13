// WallpaperEM 的 Plasma 壁纸插件（P9 spike）。
//
// 定位：plasmashell 进程内的**显示端**。渲染仍由 WallpaperEM 引擎在自己的进程里做
// （WebKitGTK + GL），插件只把引擎写出的「当前帧」贴到桌面上。这样做的原因见
// docs/kde-adaptation-plan.md §2.5/§4.1：外部窗口永远排不到 plasmashell 桌面视图
// 之下，而「壁纸 + 桌面图标同时可见」只有插件这一条路。
//
// 引擎侧契约（同目录 README.md 有完整版）：
//   * 引擎把最新帧写成 PNG，**先写 <path>.tmp 再 rename**（原子替换）；
//   * 插件只读，不写、不删、不改名；
//   * 引擎没起来 / 帧还没准备好 → 显示 FallbackColor，不报错、不阻塞 plasmashell。
//
// 为什么带 stub：`wallpaper` 是 plasmashell 注入的上下文属性，直接用 `qmlscene`
// 单跑本文件时不存在。分支后本文件可以独立渲染（scripts/kde-plugin-check.sh 用
// 它做「引擎写帧 → 插件显示」的端到端验证），也让预览/无 plasmashell 的场景不炸。
import QtQuick 2.15

Item {
    id: root

    // ---- 配置：Plasma 注入的 wallpaper.configuration，或独立运行时的桩 ----
    // 桩只能用**小写**属性名：QML 不允许自定义属性首字母大写（配置键 FramePath
    // 之类的大小写是 KConfigXT 定的，只在 Plasma 注入的 QObject 上存在）。
    QtObject {
        id: stubConfig
        property string framePath: "/tmp/wallpaperem/frame.png"
        property int refreshMs: 66
        property color fallbackColor: "#101014"
        property int fillMode: 1
    }
    readonly property var cfg: (typeof wallpaper !== "undefined" && wallpaper.configuration)
                               ? wallpaper.configuration : null
    readonly property string framePath: cfg ? String(cfg.FramePath) : stubConfig.framePath
    readonly property int refreshMs: cfg ? Number(cfg.RefreshMs) : stubConfig.refreshMs
    readonly property color fallbackColor: cfg ? cfg.FallbackColor : stubConfig.fallbackColor
    readonly property int fillMode: cfg ? Number(cfg.FillMode) : stubConfig.fillMode

    // 帧序号：只为让 Image 重新读同一个文件名（缓存穿透），不是帧率控制
    property int tick: 0

    Rectangle {
        id: backdrop
        anchors.fill: parent
        color: root.fallbackColor
    }

    Image {
        id: frame
        anchors.fill: parent
        // 0=拉伸 1=等比裁切（默认，铺满不留黑边）2=等比适应
        fillMode: root.fillMode === 0 ? Image.Stretch
                : root.fillMode === 2 ? Image.PreserveAspectFit
                                      : Image.PreserveAspectCrop
        // 帧文件被整体替换，缓存必须关掉；异步解码避免卡住合成线程
        cache: false
        asynchronous: true
        smooth: true
        visible: status === Image.Ready
        source: root.framePath === "" ? "" : "file://" + root.framePath + "?v=" + root.tick

        onStatusChanged: {
            if (status === Image.Error)
                console.warn("WallpaperEM: 帧读取失败 " + root.framePath)
        }

        // 分辨率变化（换屏/改缩放）时强制重新解码
        onWidthChanged: root.tick++
        onHeightChanged: root.tick++
    }

    Timer {
        // 引擎未运行时不必空转
        running: root.framePath !== "" && root.refreshMs > 0
        interval: Math.max(16, root.refreshMs)
        repeat: true
        onTriggered: root.tick++
    }

    // 引擎没跑起来时给一句人话，而不是一块纯黑
    Text {
        anchors.centerIn: parent
        visible: !frame.visible
        color: "#8a8a92"
        font.pixelSize: 14
        text: qsTr("WallpaperEM 引擎未运行")
    }
}
