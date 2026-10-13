// 插件配置界面（plasmashell 的「壁纸配置」对话框里显示）。
//
// 键名靠 `cfg_<main.xml 里的 entry name>` 别名绑定，由 Plasma 自动读写——
// 这是 plasmoid 的标准约定，不要自己调 file IO。
import QtQuick 2.15
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.15

ColumnLayout {
    id: page
    spacing: 12

    property alias cfg_FramePath: framePathField.text
    property alias cfg_RefreshMs: refreshSpin.value
    property alias cfg_FillMode: fillCombo.currentIndex

    Label {
        text: qsTr("帧文件路径（由 WallpaperEM 引擎写出）")
        font.bold: true
    }
    TextField {
        id: framePathField
        Layout.fillWidth: true
        placeholderText: "/tmp/wallpaperem/frame.png"
    }

    RowLayout {
        spacing: 12
        Label { text: qsTr("刷新间隔（毫秒，0 = 只读一次）") }
        SpinBox {
            id: refreshSpin
            from: 0
            to: 1000
            stepSize: 16
            editable: true
        }
    }

    RowLayout {
        spacing: 12
        Label { text: qsTr("填充方式") }
        ComboBox {
            id: fillCombo
            model: [qsTr("拉伸"), qsTr("等比裁切"), qsTr("等比适应")]
        }
    }

    Label {
        Layout.fillWidth: true
        wrapMode: Text.WordWrap
        opacity: 0.7
        text: qsTr("提示：引擎未运行或帧文件不存在时，桌面显示纯色兜底，不会影响 plasmashell 稳定性。")
    }
}
