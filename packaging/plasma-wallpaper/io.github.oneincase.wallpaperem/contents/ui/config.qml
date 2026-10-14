// 插件配置界面（系统设置 → 壁纸 → 类型：WallpaperEM → 配置按钮）。
//
// 键名靠 `cfg_<main.xml 里的 entry name>` 绑定，由 Plasma 自动读写 —— 这是
// plasmoid 的标准约定，不要自己碰文件 IO。
//
// 类型对齐（踩过的坑）：
//   * Bool  → CheckBox.checked
//   * Int   → SpinBox.value / ComboBox.currentIndex
//   * String→ TextField.text（颜色也用 String，避免 Color 类型在不同
//     QtQuick.Dialogs 版本间行为不一致）
import QtQuick 2.15
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.15

ColumnLayout {
    id: page
    spacing: 10

    property alias cfg_FramePath: framePathField.text
    property alias cfg_PerScreen: perScreenBox.checked
    property alias cfg_SeqPath: seqPathField.text
    property alias cfg_UseSeqFile: useSeqBox.checked
    property alias cfg_StallMs: stallSpin.value
    property alias cfg_RefreshMs: refreshSpin.value
    property alias cfg_FillMode: fillCombo.currentIndex
    property alias cfg_FallbackColor: fallbackColorField.text
    property alias cfg_FallbackImage: fallbackImageField.text
    property alias cfg_ShowStatusText: showTextBox.checked

    Label {
        Layout.fillWidth: true
        wrapMode: Text.WordWrap
        font.bold: true
        text: qsTr("WallpaperEM 引擎把画面写成帧文件，本插件负责显示。")
    }

    Label {
        Layout.fillWidth: true
        wrapMode: Text.WordWrap
        opacity: 0.7
        text: qsTr("先在 WallpaperEM 里开启「KDE 壁纸插件」输出，或手工跑 tools/frame-pump.py 验证。"
                   + "帧文件必须由引擎**原子替换**（先写 .tmp 再改名），插件只读。")
    }

    GroupBox {
        Layout.fillWidth: true
        title: qsTr("帧来源")
        ColumnLayout {
            anchors.fill: parent
            spacing: 8

            Label { text: qsTr("帧文件路径") }
            TextField {
                id: framePathField
                Layout.fillWidth: true
                placeholderText: "/dev/shm/wallpaperem/frame.ppm"
            }

            CheckBox {
                id: perScreenBox
                text: qsTr("每块屏幕用独立帧文件（替换路径里的 %SCREEN%）")
            }

            Label { text: qsTr("帧序号文件（自增整数，可选但推荐）") }
            TextField {
                id: seqPathField
                Layout.fillWidth: true
                placeholderText: "/dev/shm/wallpaperem/frame.seq"
            }
            CheckBox {
                id: useSeqBox
                text: qsTr("用序号文件决定何时重新解码（省 CPU）")
            }
            RowLayout {
                spacing: 8
                Label { text: qsTr("多久没新帧算引擎停止") }
                SpinBox {
                    id: stallSpin
                    from: 500
                    to: 60000
                    stepSize: 500
                    editable: true
                }
                Label { text: qsTr("毫秒") }
            }
        }
    }

    GroupBox {
        Layout.fillWidth: true
        title: qsTr("显示")
        ColumnLayout {
            anchors.fill: parent
            spacing: 8

            RowLayout {
                spacing: 8
                Label { text: qsTr("刷新间隔") }
                SpinBox {
                    id: refreshSpin
                    from: 0
                    to: 5000
                    stepSize: 40
                    editable: true
                }
                Label { text: qsTr("毫秒（0 = 只读一次）") }
            }

            RowLayout {
                spacing: 8
                Label { text: qsTr("填充方式") }
                ComboBox {
                    id: fillCombo
                    model: [qsTr("拉伸"), qsTr("等比裁切"), qsTr("等比适应")]
                }
            }

            Label { text: qsTr("帧不可用时的背景色") }
            TextField {
                id: fallbackColorField
                Layout.preferredWidth: 160
                placeholderText: "#101014"
            }

            Label { text: qsTr("帧不可用时的静态图片（可留空）") }
            TextField {
                id: fallbackImageField
                Layout.fillWidth: true
                placeholderText: qsTr("/home/you/Pictures/wallpaper.jpg")
            }

            CheckBox {
                id: showTextBox
                text: qsTr("显示「引擎未运行 / 已停止」提示文字")
            }
        }
    }
}
