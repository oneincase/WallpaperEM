// 辅助功能（AX）驱动：按**标题**按下界面里的按钮，用来切页而不动鼠标。
//
// 为什么不坐标点击：顶部胶囊导航选中/悬浮时会横向拉长（grid 0fr→1fr），
// 点到一半布局就变了（移动靶）；AX 只改焦点/派发点击，不移动指针、不抢前台应用。
//
// 用法（pid 用 `pgrep -x WallpaperEM` 取）：
//   axdrive <pid> tree [maxDepth]              # 打 AX 树（排查「按钮叫什么名字」）
//   axdrive <pid> list [yMin yMax]             # 列按钮（可按屏幕纵向范围过滤）
//   axdrive <pid> press <title> [yMin yMax]    # 按下标题匹配的按钮（精确匹配优先，其次 y 更靠上）
//
// 纵向范围过滤是给导航条用的：胶囊固定在窗口顶部（窗口 y + 6..48），
// 同名文案（如「分享」）在页面正文里也可能出现，限定范围才不会按错。
import ApplicationServices
import Foundation

func attr(_ el: AXUIElement, _ n: String) -> CFTypeRef? {
    var v: CFTypeRef?
    return AXUIElementCopyAttributeValue(el, n as CFString, &v) == .success ? v : nil
}
func s(_ el: AXUIElement, _ n: String) -> String { (attr(el, n) as? String) ?? "" }
func children(_ el: AXUIElement) -> [AXUIElement] {
    (attr(el, kAXChildrenAttribute as String) as? [AXUIElement]) ?? []
}
/// 应用的窗口要用 kAXWindowsAttribute 取（kAXChildren 在部分 app 上不含窗口）
func windows(_ app: AXUIElement) -> [AXUIElement] {
    if let ws = attr(app, kAXWindowsAttribute as String) as? [AXUIElement], !ws.isEmpty { return ws }
    return children(app).filter { s($0, kAXRoleAttribute as String) == kAXWindowRole as String }
}
func frame(_ el: AXUIElement) -> CGRect {
    var p = CGPoint.zero, sz = CGSize.zero
    if let v = attr(el, kAXPositionAttribute as String) { AXValueGetValue(v as! AXValue, .cgPoint, &p) }
    if let v = attr(el, kAXSizeAttribute as String) { AXValueGetValue(v as! AXValue, .cgSize, &sz) }
    return CGRect(x: p.x, y: p.y, width: sz.width, height: sz.height)
}

let args = CommandLine.arguments
guard args.count >= 3 else {
    FileHandle.standardError.write("usage: axdrive <pid> tree [depth] | list [yMin yMax] | press <title> [yMin yMax]\n".data(using: .utf8)!)
    exit(2)
}
let app = AXUIElementCreateApplication(pid_t(Int(args[1])!))
let mode = args[2]
var yMin = -1e9, yMax = 1e9
if mode == "list", args.count >= 5 {
    yMin = Double(args[3]) ?? -1e9; yMax = Double(args[4]) ?? 1e9
} else if mode == "press", args.count >= 6 {
    yMin = Double(args[4]) ?? -1e9; yMax = Double(args[5]) ?? 1e9
}

if mode == "tree" {
    let maxDepth = args.count > 3 ? Int(args[3])! : 5
    func walk(_ el: AXUIElement, _ d: Int) {
        if d > maxDepth { return }
        let role = s(el, kAXRoleAttribute as String)
        let title = s(el, kAXTitleAttribute as String)
        let value = (attr(el, kAXValueAttribute as String) as? String)?.prefix(60) ?? ""
        let f = frame(el)
        print("\(String(repeating: "  ", count: d))\(role) \"\(title)\" val=\"\(value)\" @\(Int(f.minX)),\(Int(f.minY)) \(Int(f.width))x\(Int(f.height))")
        for c in children(el) { walk(c, d + 1) }
    }
    var n = 0
    for w in windows(app) where s(w, kAXRoleAttribute as String) == kAXWindowRole as String {
        n += 1
        print("WINDOW \"\(s(w, kAXTitleAttribute as String))\"")
        walk(w, 0)
    }
    print("windows=\(n)")
    exit(0)
}

struct Hit { let el: AXUIElement; let role: String; let title: String; let desc: String; let rect: CGRect; let exact: Bool }
var hits: [Hit] = []
let want: String? = mode == "press" ? args[3] : nil

func collect(_ el: AXUIElement, _ depth: Int) {
    if depth > 45 { return }
    let role = s(el, kAXRoleAttribute as String)
    let clickable = role == "AXButton" || role == "AXLink" || role == "AXRadioButton" || role == "AXCheckBox"
    let r = frame(el)
    if clickable, r.height > 0, r.midY >= yMin, r.midY <= yMax {
        let title = s(el, kAXTitleAttribute as String)
        let desc = s(el, kAXDescriptionAttribute as String)
        let value = (attr(el, kAXValueAttribute as String) as? String) ?? ""
        if let w = want {
            let exact = title == w || desc == w || value == w
            if exact || title.contains(w) || desc.contains(w) || value.contains(w) {
                hits.append(Hit(el: el, role: role, title: title, desc: desc, rect: r, exact: exact))
            }
        } else {
            hits.append(Hit(el: el, role: role, title: title, desc: desc, rect: r, exact: true))
        }
    }
    for c in children(el) { collect(c, depth + 1) }
}
for w in windows(app) where s(w, kAXRoleAttribute as String) == kAXWindowRole as String { collect(w, 0) }

if mode == "list" {
    for h in hits {
        print("\(h.role) title=\"\(h.title)\" desc=\"\(h.desc)\" @\(Int(h.rect.minX)),\(Int(h.rect.minY)) \(Int(h.rect.width))x\(Int(h.rect.height))")
    }
    print("total=\(hits.count)")
} else if mode == "press" {
    // 精确匹配优先，其次 y 更靠上的（导航条在顶部）
    let ordered = hits.sorted { a, b in a.exact != b.exact ? a.exact : a.rect.minY < b.rect.minY }
    guard let t = ordered.first else {
        FileHandle.standardError.write("not found: \(args[3])\n".data(using: .utf8)!)
        exit(1)
    }
    let ok = AXUIElementPerformAction(t.el, kAXPressAction as CFString) == .success
    print("press \(t.role) \"\(t.title)\" @\(Int(t.rect.minX)),\(Int(t.rect.minY)) \(Int(t.rect.width))x\(Int(t.rect.height)) -> \(ok ? "ok" : "FAILED")")
    exit(ok ? 0 : 1)
} else {
    FileHandle.standardError.write("unknown mode: \(mode)\n".data(using: .utf8)!)
    exit(2)
}
