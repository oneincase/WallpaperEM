// 列出本应用（默认 owner=WallpaperEM）的窗口：id / layer / 是否在屏 / 标题 / 边界。
//
// 用途：抓界面截图前拿主窗口的 CGWindowList window number —— `screencapture -l <id>`
// 按窗口抓，而不是按屏幕区域抓（后者会被别的窗口盖住）。
//
//   windowlist            # 全部窗口（含桌面层壁纸窗口）
//   windowlist --main     # 只打印「在屏的主窗口」：`id x y w h`（没有则什么都不打印，退出码 1）
//   windowlist --owner X  # 换 owner 名（别的 app 也能看）
import CoreGraphics
import Foundation

let args = Array(CommandLine.arguments.dropFirst())
var owner = "WallpaperEM"
var mainOnly = false
var i = 0
while i < args.count {
    switch args[i] {
    case "--owner":
        i += 1
        if i < args.count { owner = args[i] }
    case "--main":
        mainOnly = true
    default:
        FileHandle.standardError.write("unknown arg: \(args[i])\n".data(using: .utf8)!)
        exit(2)
    }
    i += 1
}

// .optionAll：窗口在别的 Space / 被完全遮挡时也要列出来（才能判断「为什么抓出来是空的」）
guard let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] else {
    exit(1)
}

var found = false
for w in list {
    guard (w[kCGWindowOwnerName as String] as? String ?? "") == owner else { continue }
    let num = w[kCGWindowNumber as String] as? Int ?? -1
    let layer = w[kCGWindowLayer as String] as? Int ?? -999
    let onscreen = (w[kCGWindowIsOnscreen as String] as? Bool) ?? false
    let name = w[kCGWindowName as String] as? String ?? ""
    let b = w[kCGWindowBounds as String] as? [String: Any] ?? [:]
    let x = b["X"] as? Double ?? 0, y = b["Y"] as? Double ?? 0
    let wd = b["Width"] as? Double ?? 0, ht = b["Height"] as? Double ?? 0

    // 主窗口 = layer 0 + 标题是应用名（桌面层壁纸窗口的 layer 是负数，排除）
    let isMain = layer == 0 && onscreen && name == owner
    if mainOnly {
        // 打印 id + 边界：调用方要按窗口原点推导航条的纵向范围（胶囊在窗口顶部）
        if isMain {
            print("\(num) \(Int(x)) \(Int(y)) \(Int(wd)) \(Int(ht))")
            found = true
            break
        }
        continue
    }
    print("id=\(num) layer=\(layer) onscreen=\(onscreen ? 1 : 0) name=\"\(name)\" bounds=\(Int(x)),\(Int(y)) \(Int(wd))x\(Int(ht))\(isMain ? "  <- main" : "")")
}
exit(found || !mainOnly ? 0 : 1)
