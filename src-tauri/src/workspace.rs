//! AI 壁纸工程工作区（MCP）：工程目录管理 + WE 工程校验 + scene.pkg 打包。
//!
//! 工程根：`<系统文档目录>/WallpaperEM/Projects/<工程名>/`
//! 选文档目录而不是应用数据目录，是为了让用户能直接拿编辑器/Finder 改，
//! 也让 agent 用自己的文件工具往工程里丢素材（图片等）。
//!
//! 本模块不做任何渲染：产出的 scene.pkg 与 project.json 走的都是既有链路
//! （内容服务器 → webwallgl 渲染器），安装/应用见 library.rs 与 wallpaper/。
//!
//! 打包格式（对照真实语料逐字段核对，勿凭印象改）：
//! - scene.pkg：`u32 magicLen` + `"PKGV00xx"` + `u32 count` + 入口表
//!   （`u32 nameLen` + name + `u32 offset` + `u32 size`，offset 相对数据段起点）
//!   + 数据段。解析方只按名字取入口，不要求顺序或对齐。
//! - .tex：`TEXV0005\0` + `TEXI0001\0` + format/flags/W/H/W/H/ignored
//!   + `TEXB0004\0` + imageCount + freeImageFormat + hasMipExtension
//!     + 每个 mip：`mipCount` + `mw mh` + `compression` + `uncompressedSize`
//!     + `compressedSize` + 数据。
//!   + hasMipExtension=0 → 解析方按 V3 布局读（见 vendor 的 parseTex）。
//!   + freeImageFormat=13(PNG)/2(JPEG) 时数据段直接是整张图片文件，无需解码重编码。
//!   + **不要用 WebP**：`FIF.WEBP === 35 === FIF.MP4`，解析方会把它当视频纹理吞掉。

use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

/// 工程根目录名（文档目录下一级）
const PROJECTS_DIRNAME: &str = "WallpaperEM";
const PROJECTS_SUBDIR: &str = "Projects";
/// 版本历史目录：`.version/<版本号>/` 存该版源文件的完整副本。
/// MCP 只负责**排除**它（不进包、不进本地库、不许写入）；历史由创作者自己维护。
pub(crate) const VERSION_DIR: &str = ".version";
/// 每个版本目录里的元数据（隐藏文件，树遍历一律跳过，不会混进工程/包/本地库）
/// 年龄分级标签（与 `src/lib/tags.ts` 的「年龄分级」组同口径）。
/// 值必须与 Steam 工坊标签**逐字符一致**：它们既走筛选也随发布带给 Steam。
pub(crate) const RATING_TAGS: [&str; 3] = ["Everyone", "Questionable", "Mature"];
/// 默认分级：大众级
pub(crate) const DEFAULT_RATING: &str = "Everyone";
/// 单个工程目录名长度上限（含中文按字符数算）
const MAX_PROJECT_NAME_CHARS: usize = 64;
/// 工程内单文件写入上限（base64 解码后）。
///
/// 必须**小于 MCP 侧的 JSON 请求体上限**（`mcp::MCP_MAX_BODY_BYTES`，64 MiB）：
/// base64 会膨胀 4/3，40 MiB 原始数据编成 base64 加 JSON 外壳约 54 MiB，还留了余量。
/// 反过来定的后果是 agent 撞在传输层（HTTP 413 的纯文本错误），看不到这条可操作的
/// 提示 ——「说好的 512 MiB 呢」就是这么来的。
const MAX_WRITE_BYTES: usize = 40 << 20;

/// 上面的 MB 口径，供 MCP 侧的错误提示复用（避免两处各写一个数字）
pub const MAX_WRITE_MB: usize = MAX_WRITE_BYTES >> 20;
/// 打包时工程总大小上限（防止把整个图库塞进一个 pkg）
const MAX_PACK_BYTES: u64 = 2 << 30;
const PKG_MAGIC: &str = "PKGV0022";

/// 粒子发射器：渲染库**只特判 `boxrandom`**，其余任何名字都按球壳发射
/// （WE 里那个名字就是 `sphererandom`，但代码里没有它的字面量）。
/// 所以这里不是白名单 —— 校验器只在 `name` 缺失时提醒，见 `check_particle_layer`。
pub(crate) const PARTICLE_EMITTERS: [&str; 1] = ["boxrandom"];
/// 粒子 initializer：与渲染库 `switch (name)` 的分支**逐个对齐**
/// （`renderer/vendor/we-scene/render/particles.js`；名单外的名字被静默忽略）。
pub(crate) const PARTICLE_INITIALIZERS: [&str; 9] = [
    "lifetimerandom",
    "sizerandom",
    "colorrandom",
    "alpharandom",
    "velocityrandom",
    "rotationrandom",
    "angularvelocityrandom",
    "mapsequencearoundcontrolpoint",
    "mapsequencebetweencontrolpoints",
];
/// 粒子 operator：同上，与库的 switch 分支对齐
pub(crate) const PARTICLE_OPERATORS: [&str; 20] = [
    "movement",
    "angularmovement",
    "alphafade",
    "alphachange",
    "sizechange",
    "colorchange",
    "turbulence",
    "oscillatealpha",
    "oscillatesize",
    "oscillateposition",
    "controlpointattract",
    "vortex",
    "vortex_v2",
    "remapvalue",
    "capvelocity",
    "positionoffsetrandom",
    "collisionquad",
    "collisionplane",
    "reducemovementnearcontrolpoint",
    "maintaindistancebetweencontrolpoints",
];
/// 粒子 renderer：`sprite` 是缺省（名字不认识也按 sprite 画），另外三种是尾迹
pub(crate) const PARTICLE_RENDERERS: [&str; 4] = ["sprite", "spritetrail", "ropetrail", "rope"];

/// 允许「声明了但没有图层绑定」的 WE 约定属性：`schemecolor`
/// （`ui_browse_properties_scheme_color`）是工坊浏览页的配色，不参与场景渲染。
const UNBOUND_OK_PROPS: [&str; 1] = ["schemecolor"];

// ---------------------------------------------------------------- 模板

/// 内置模板文件清单（路径相对工程根）。文本模板里的 `{{TITLE}}` 会被工程标题替换。
fn template_files(kind: &str) -> Option<Vec<(&'static str, &'static [u8])>> {
    match kind {
        "web" => Some(vec![
            (
                "project.json",
                include_bytes!("../templates/web/project.json"),
            ),
            ("index.html", include_bytes!("../templates/web/index.html")),
            ("style.css", include_bytes!("../templates/web/style.css")),
            ("main.js", include_bytes!("../templates/web/main.js")),
        ]),
        "scene" => Some(vec![
            (
                "project.json",
                include_bytes!("../templates/scene/project.json"),
            ),
            ("scene.json", include_bytes!("../templates/scene/scene.json")),
            (
                "models/background.json",
                include_bytes!("../templates/scene/models/background.json"),
            ),
            (
                "materials/background.json",
                include_bytes!("../templates/scene/materials/background.json"),
            ),
            (
                "materials/bg.png",
                include_bytes!("../templates/scene/materials/bg.png"),
            ),
            (
                "materials/presets/fireflies.json",
                include_bytes!("../templates/scene/materials/presets/fireflies.json"),
            ),
            (
                "particles/presets/fireflies.json",
                include_bytes!("../templates/scene/particles/presets/fireflies.json"),
            ),
        ]),
        _ => None,
    }
}

/// 模板文件名清单（MCP 资源用，不含内容）
pub fn template_kinds() -> [&'static str; 2] {
    ["web", "scene"]
}

/// 读取模板文件（MCP 资源用）。返回 `(相对路径, 字节)`。
pub fn template_resource(kind: &str) -> Option<Vec<(&'static str, &'static [u8])>> {
    template_files(kind)
}

// ---------------------------------------------------------------- 路径安全

/// 工程根目录 `<文档>/WallpaperEM/Projects`
pub fn projects_root(_app: &AppHandle) -> Result<PathBuf, String> {
    let base = dirs::document_dir()
        .ok_or("无法定位系统文档目录（MCP 工程工作区不可用）".to_string())?
        .join(PROJECTS_DIRNAME)
        .join(PROJECTS_SUBDIR);
    std::fs::create_dir_all(&base).map_err(|e| format!("创建工作区失败: {e}"))?;
    Ok(base)
}

/// 模板文件落盘用的内容：**文本**模板替换 `{{TITLE}}`，其余**原样**拷贝。
///
/// 必须按「是不是合法 UTF-8」分流：`String::from_utf8_lossy` 会把 PNG 里的
/// 0x89 之类的字节换成 U+FFFD，贴图落盘即损坏（症状是 scene_pack 报「不是合法 PNG」）。
fn template_body(bytes: &[u8], title: &str) -> Vec<u8> {
    match std::str::from_utf8(bytes) {
        Ok(text) if text.contains("{{TITLE}}") => text.replace("{{TITLE}}", title).into_bytes(),
        _ => bytes.to_vec(),
    }
}

/// 工程名校验：工程名即目录名，同时也是 MCP 里引用工程的 id
fn check_project_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("工程名不能为空".into());
    }
    if n.chars().count() > MAX_PROJECT_NAME_CHARS {
        return Err(format!("工程名过长（上限 {MAX_PROJECT_NAME_CHARS} 字符）"));
    }
    if n.starts_with('.') {
        return Err("工程名不能以 . 开头".into());
    }
    if n.contains('/') || n.contains('\\') {
        return Err("工程名不能包含路径分隔符".into());
    }
    if n.contains(':') || n.chars().any(|c| c.is_control()) {
        return Err("工程名不能包含冒号或控制字符".into());
    }
    if n == "." || n == ".." {
        return Err("工程名非法".into());
    }
    Ok(())
}

/// 工程目录（不创建）
pub fn project_dir(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    check_project_name(name)?;
    Ok(projects_root(app)?.join(name.trim()))
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// `.version/` 是**创作者自己的**版本历史目录（MCP 不再代管快照/回退：app 里那三个
/// 工具已随方案调整移除）。允许 agent 直接往里写，等于把「第 N 版」改成谁也不知道
/// 是什么的半成品；这里一律拒写，历史由创作者用 git 或自己的方式维护。
///
/// 只看**第一个有效路径段**（跳过空段与 `.`），否则 `./.version/1/x` 这种写法能绕过检查。
fn reject_version_path(rel: &str) -> Result<(), String> {
    let norm = rel.trim().replace('\\', "/");
    let first = norm.split('/').find(|s| !s.is_empty() && *s != ".");
    if first == Some(VERSION_DIR) {
        return Err(format!(
            "{VERSION_DIR}/ 是创作者自己的版本历史目录，不能用 project_write_file 写；\
             历史请用 git 或自行拷贝目录维护（打包/安装也不会带上它）"
        ));
    }
    Ok(())
}

/// 把工程内的相对路径解析成绝对路径，并保证逃不出工程根。
///
/// 三道闸：拒绝绝对路径/盘符、拒绝 `..`、逐级拒绝符号链接（否则
/// `ln -s /etc passwd` 之后写 `passwd/hosts` 就能改到工程外）。
pub(crate) fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let norm = rel.trim().replace('\\', "/");
    if norm.is_empty() {
        return Err("路径不能为空".into());
    }
    if norm.starts_with('/') {
        return Err("只接受工程内相对路径".into());
    }
    if norm.len() > 1 && norm.as_bytes().get(1) == Some(&b':') {
        return Err("只接受工程内相对路径".into());
    }
    let mut out = root.to_path_buf();
    for seg in norm.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return Err("路径不能包含 ..".into());
        }
        out.push(seg);
        if is_symlink(&out) {
            return Err(format!("路径包含符号链接，已拒绝: {rel}"));
        }
    }
    if !out.starts_with(root) {
        return Err("路径逃出工程目录".into());
    }
    Ok(out)
}

/// 工程内的相对路径（用于展示/报错）；不在工程内时返回原名
pub(crate) fn rel_display(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| p.to_string_lossy().to_string())
}

// ---------------------------------------------------------------- 工程读写

/// 列出工作区里的全部工程
/// 工作区里每个工程对应的**库条目 id**（`Projects/` 是「项目」这条筛选的真源）。
///
/// 为什么不看库表：库里没有"这行是不是自建工程"的列 —— 工坊下载的条目 id 是一串数字，
/// 自建工程的 id 由工程名清洗而来，两者只能靠"有没有对应工程目录"区分。工程目录在，
/// 就算项目；工程删了它自然不再是项目（那时它就是一条普通库条目）。
pub fn project_item_ids(app: &AppHandle) -> Vec<String> {
    let Ok(root) = projects_root(app) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() || is_symlink(&path) {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        out.push(crate::library::project_item_id_for(&name));
    }
    out.sort();
    out.dedup();
    out
}

pub fn list_projects(app: &AppHandle) -> Result<Value, String> {
    let root = projects_root(app)?;
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) => return Err(format!("读取工作区失败: {e}")),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || is_symlink(&path) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let (wtype, title) = read_project_meta(&path);
        let packed = path.join("scene.pkg").is_file();
        let installed = installed_item_id(app, &name);
        out.push(json!({
            "project": name,
            "path": path.to_string_lossy(),
            "type": wtype,
            "title": title,
            "packed": packed,
            "installedItemId": installed,
            "updatedAt": entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }));
    }
    out.sort_by(|a, b| a["project"].as_str().cmp(&b["project"].as_str()));
    Ok(json!({ "root": root.to_string_lossy(), "projects": out }))
}

/// 本地库里已安装的工程条目 id（`custom-<hash>` 目录里带 `.wpem-project` 标记文件）
///
/// 导入时会连同工程目录一起拷进本地库；标记文件在导入后仍能被读到，
/// 于是「这个工程装了没」不需要额外的数据库字段。
fn installed_item_id(app: &AppHandle, project: &str) -> Option<String> {
    let id = crate::library::project_item_id_for(project);
    let dir = crate::library::wallpapers_dir(app).ok()?.join(&id);
    dir.is_dir().then_some(id)
}

/// 上面那位的公开版（MCP 的 `wallpaper_preview` 要按工程名找库里那份副本）
pub fn installed_item_id_of(app: &AppHandle, project: &str) -> Option<String> {
    installed_item_id(app, project)
}

fn read_project_meta(dir: &Path) -> (String, String) {
    let Ok(text) = std::fs::read_to_string(dir.join("project.json")) else {
        return ("unknown".into(), String::new());
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return ("unknown".into(), String::new());
    };
    (
        v.get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown")
            .to_string(),
        v.get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
    )
}

/// 新建工程（模板 -> 工程目录）。已存在时只有 `force=true` 才覆盖。
pub fn create_project(
    app: &AppHandle,
    name: &str,
    kind: &str,
    title: Option<&str>,
    force: bool,
) -> Result<Value, String> {
    check_project_name(name)?;
    let kind = kind.trim().to_ascii_lowercase();
    let files = template_files(&kind).ok_or_else(|| {
        format!(
            "未知模板类型 `{kind}`（可用：{}）",
            template_kinds().join(" / ")
        )
    })?;
    let root = projects_root(app)?;
    let dir = root.join(name.trim());
    if dir.exists() && !force {
        return Err(format!(
            "工程 `{}` 已存在（要覆盖请传 force=true）",
            name.trim()
        ));
    }
    if dir.exists() && force {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("覆盖旧工程失败: {e}"))?;
    }
    let title = title
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .unwrap_or(name.trim())
        .to_string();
    let mut written = Vec::new();
    for (rel, bytes) in files {
        let body = template_body(bytes, &title);
        let dest = safe_join(&dir, rel)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&dest, &body).map_err(|e| format!("写入 {rel} 失败: {e}"))?;
        written.push(rel.to_string());
    }
    Ok(json!({
        "project": name.trim(),
        "path": dir.to_string_lossy(),
        "type": kind,
        "title": title,
        "files": written,
        "nextSteps": if kind == "scene" {
            json!(["改 scene.json / 换 materials 里的贴图", "scene_pack 打包", "project_install 安装", "wallpaper_screenshot 看效果"])
        } else {
            json!(["改 index.html / main.js", "project_install 安装", "wallpaper_screenshot 看效果"])
        },
    }))
}

pub fn delete_project(app: &AppHandle, name: &str) -> Result<Value, String> {
    let dir = project_dir(app, name)?;
    if !dir.is_dir() {
        return Err(format!("工程不存在: {}", name.trim()));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("删除失败: {e}"))?;
    Ok(json!({ "deleted": name.trim() }))
}

/// 写入工程内文件。`encoding`: "utf8"（默认）| "base64"
pub fn write_project_file(
    app: &AppHandle,
    name: &str,
    rel: &str,
    content: &str,
    encoding: &str,
) -> Result<Value, String> {
    let dir = project_dir(app, name)?;
    if !dir.is_dir() {
        return Err(format!("工程不存在: {}", name.trim()));
    }
    reject_version_path(rel)?;
    let dest = safe_join(&dir, rel)?;
    let bytes = match encoding.trim().to_ascii_lowercase().as_str() {
        "" | "utf8" | "text" => content.as_bytes().to_vec(),
        "base64" => B64
            .decode(content.trim())
            .map_err(|e| format!("base64 解码失败: {e}"))?,
        other => return Err(format!("不支持的 encoding: {other}（可用 utf8 / base64）")),
    };
    if bytes.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "文件过大（{} > 上限 {}）",
            bytes.len(),
            MAX_WRITE_BYTES
        ));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&dest, &bytes).map_err(|e| format!("写入失败: {e}"))?;
    // 改过素材就等于旧包过期：删掉打包产物，避免安装到旧场景
    let stale_pkg = dir.join("scene.pkg");
    let invalidated = stale_pkg.is_file();
    if invalidated {
        let _ = std::fs::remove_file(&stale_pkg);
    }
    let (removed_stale_tex, texture_warning) = resolve_texture_siblings(&dir, &dest, rel);
    Ok(json!({
        "project": name.trim(),
        "path": rel_display(&dir, &dest),
        "bytes": bytes.len(),
        "packInvalidated": invalidated,
        // 顺手清掉的同名 .tex（见 resolve_texture_siblings）
        "removedStaleTex": removed_stale_tex,
        // 非空 = 这份写入会和另一份同名贴图打架，必须让 agent 看见
        "warning": texture_warning,
    }))
}

/// 写入贴图时的同名兄弟文件处理（返回 `(清掉的 .tex 名, 需要提醒的话)`）。
///
/// 打包器对 `materials/<name>` 只认一份：`.tex` 存在就用 `.tex`、源图被忽略。于是
/// 「先手写了 .tex，后来改用 .png」这条路径会**静默用回旧贴图** —— 画面和源图不一致
/// 且毫无提示。这里按写入方向做两件事：
///   - 写源图（png/jpg/jpeg）→ 删掉同名 `.tex`（源图是唯一真源，.tex 只是打包中间物）
///   - 写 `.tex` → 不动源图（那是用户的素材），但回一句提醒
fn resolve_texture_siblings(dir: &Path, dest: &Path, rel: &str) -> (Value, Value) {
    let rel = rel.replace('\\', "/");
    let Some(name) = rel.strip_prefix("materials/") else {
        return (Value::Null, Value::Null);
    };
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return (Value::Null, Value::Null);
    };
    let ext = ext.to_ascii_lowercase();
    if stem.is_empty() || stem.contains('/') {
        return (Value::Null, Value::Null);
    }
    let has_src = ["png", "jpg", "jpeg"]
        .iter()
        .find(|e| dir.join(format!("materials/{stem}.{e}")).is_file())
        .copied();
    if matches!(ext.as_str(), "png" | "jpg" | "jpeg") {
        let tex = dir.join(format!("materials/{stem}.tex"));
        if !tex.is_file() {
            return (Value::Null, Value::Null);
        }
        let removed = std::fs::remove_file(&tex).is_ok();
        let note = if removed {
            format!("已删除同名的 materials/{stem}.tex（打包会从这份源图重新转换）")
        } else {
            format!("materials/{stem}.tex 与这份源图同名且删不掉，请手动删除后再打包")
        };
        return (if removed { json!(format!("materials/{stem}.tex")) } else { Value::Null }, json!(note));
    }
    if ext == "tex" {
        if let Some(src_ext) = has_src {
            if rel_display(dir, dest) != format!("materials/{stem}.{src_ext}") {
                return (
                    Value::Null,
                    json!(format!(
                        "materials/{stem}.tex 会盖住同名的 materials/{stem}.{src_ext}（打包优先用 .tex）；\
                         想用源图就先删掉这份 .tex"
                    )),
                );
            }
        }
    }
    (Value::Null, Value::Null)
}

pub fn read_project_file(
    app: &AppHandle,
    name: &str,
    rel: &str,
    max_bytes: usize,
) -> Result<Value, String> {
    let dir = project_dir(app, name)?;
    let path = safe_join(&dir, rel)?;
    let meta = std::fs::metadata(&path).map_err(|_| format!("文件不存在: {rel}"))?;
    if !meta.is_file() {
        return Err(format!("不是文件: {rel}"));
    }
    let limit = if max_bytes == 0 { 1 << 20 } else { max_bytes };
    if meta.len() as usize > limit {
        return Err(format!(
            "文件过大（{} 字节 > 上限 {limit}），请用 project_list_files 查看或直接读工程目录",
            meta.len()
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let is_text = std::str::from_utf8(&bytes).is_ok();
    if is_text {
        Ok(json!({
            "path": rel_display(&dir, &path),
            "encoding": "utf8",
            "content": String::from_utf8_lossy(&bytes),
        }))
    } else {
        Ok(json!({
            "path": rel_display(&dir, &path),
            "encoding": "base64",
            "content": B64.encode(&bytes),
        }))
    }
}

pub fn list_project_files(app: &AppHandle, name: &str) -> Result<Value, String> {
    let dir = project_dir(app, name)?;
    if !dir.is_dir() {
        return Err(format!("工程不存在: {}", name.trim()));
    }
    let mut files = Vec::new();
    let mut total: u64 = 0;
    collect_files(&dir, &dir, &mut files, &mut total)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let list: Vec<Value> = files
        .into_iter()
        .map(|(rel, len)| json!({ "path": rel, "bytes": len }))
        .collect();
    Ok(json!({
        "project": name.trim(),
        "path": dir.to_string_lossy(),
        "fileCount": list.len(),
        "totalBytes": total,
        "files": list,
    }))
}

/// 递归收集工程内文件（相对路径, 字节）。跳过隐藏项与打包产物。
fn collect_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, u64)>,
    total: &mut u64,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("读取目录失败: {e}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "scene.pkg" {
            continue;
        }
        if is_symlink(&path) {
            continue;
        }
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        if ft.is_dir() {
            collect_files(root, &path, out, total)?;
        } else if ft.is_file() {
            let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
            *total += len;
            out.push((rel_display(root, &path), len));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- project.json 读取 / 标签 / 年龄分级

/// 读工程根下的 project.json（解析失败当作没有）
pub(crate) fn read_project_json(dir: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(dir.join("project.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// 版本号：`project.json` 里的 `version`，必须是 ≥ 1 的整数。
///
/// 它同时是创作者版历史 `.version/<n>/` 与工坊发布（更新已发布条目时会读它）的
/// 版本标识，所以不接受字符串（`"1.0.0"` 没法当目录名排序，版本比较也会变成字符串比较）。
fn project_version(project: &Value) -> Result<u64, String> {
    match project.get("version") {
        Some(Value::Number(n)) => match n.as_u64() {
            Some(v) if v >= 1 => Ok(v),
            _ => Err("project.json 的 version 必须是 ≥ 1 的整数".into()),
        },
        Some(_) => Err("project.json 的 version 必须是整数（不要写成字符串）".into()),
        None => Err("project.json 缺少 version（≥ 1 的整数；发布与版本历史都靠它）".into()),
    }
}

/// project.json 的分类标签（非字符串项丢掉）
pub(crate) fn project_tags(project: &Value) -> Vec<String> {
    project
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// 年龄分级校验：分类标签里必须有、且只能有一个分级标签
fn check_rating(project: &Value, errors: &mut Vec<String>) {
    let tags = project_tags(project);
    let ratings: Vec<&String> = tags
        .iter()
        .filter(|t| RATING_TAGS.contains(&t.as_str()))
        .collect();
    match ratings.len() {
        0 => errors.push(format!(
            "project.json 的 tags 缺少年龄分级标签（必须是 {} 之一，默认 {DEFAULT_RATING}=大众级）",
            RATING_TAGS.join(" / ")
        )),
        1 => {}
        _ => errors.push(format!(
            "project.json 的 tags 有多个年龄分级标签（{}），只能留一个",
            ratings
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        )),
    }
}

// ---------------------------------------------------------------- 校验

/// project.json / scene.json 静态校验。返回 `{ ok, type, title, entry, errors, warnings }`
pub fn validate_project(app: &AppHandle, name: &str) -> Result<Value, String> {
    let dir = project_dir(app, name)?;
    if !dir.is_dir() {
        return Err(format!("工程不存在: {}", name.trim()));
    }
    Ok(validate_dir(&dir, name.trim()))
}

pub fn validate_dir(dir: &Path, name: &str) -> Value {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let project_path = dir.join("project.json");
    let Some(text) = std::fs::read_to_string(&project_path).ok() else {
        return json!({
            "ok": false, "project": name, "type": "unknown", "title": "",
            "errors": ["缺少 project.json（工程根必须有它）"], "warnings": [],
        });
    };
    let parsed: Result<Value, _> = serde_json::from_str(&text);
    let Ok(project) = parsed else {
        return json!({
            "ok": false, "project": name, "type": "unknown", "title": "",
            "errors": ["project.json 不是合法 JSON"], "warnings": [],
        });
    };

    let wtype = project
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let title = project
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    if wtype.is_empty() {
        errors.push("project.json 缺少 type（web / scene / video / gif / image）".into());
    } else if !matches!(
        wtype.as_str(),
        "web" | "scene" | "video" | "gif" | "image"
    ) {
        errors.push(format!("project.json 的 type 不支持: {wtype}"));
    }
    if title.trim().is_empty() {
        warnings.push("project.json 没有 title（本地库里会显示目录名）".into());
    }

    // 版本号与年龄分级：发布/回退/本地库筛选都依赖它们，缺了不给过
    if let Err(e) = project_version(&project) {
        errors.push(e);
    }
    check_rating(&project, &mut errors);

    // file 字段：相对路径 + 文件真实存在
    if let Some(rel) = project.get("file").and_then(|f| f.as_str()) {
        match safe_join(dir, rel) {
            Ok(p) if p.is_file() => {}
            Ok(_) => errors.push(format!("project.json 的 file 指向的文件不存在: {rel}")),
            Err(e) => errors.push(format!("project.json 的 file 非法: {e}")),
        }
    }

    check_properties(&project, &mut errors, &mut warnings);

    let entry = match wtype.as_str() {
        "web" => {
            let e = web_entry(dir, &project);
            if e.is_none() {
                errors.push("找不到网页入口：请提供 project.json 的 file，或 index.html / web/index.html".into());
            }
            e
        }
        "scene" => {
            let scene_path = dir.join("scene.json");
            if !scene_path.is_file() {
                errors.push("缺少 scene.json".into());
            } else {
                match std::fs::read_to_string(&scene_path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                {
                    Some(scene) => {
                        check_scene(dir, &project, &scene, &mut errors, &mut warnings)
                    }
                    None => errors.push("scene.json 不是合法 JSON".into()),
                }
            }
            if !dir.join("scene.pkg").is_file() {
                warnings.push("还没有 scene.pkg，安装前请先调用 scene_pack 打包".into());
            }
            project.get("file").and_then(|f| f.as_str()).map(String::from)
        }
        _ => project.get("file").and_then(|f| f.as_str()).map(String::from),
    };

    // 同一张贴图/同一个模型常被多个图层引用，逐引用收集会把同一条报很多遍；
    // 这里按首次出现去重（顺序保持不变），报告才读得下去。
    dedupe(&mut errors);
    dedupe(&mut warnings);

    json!({
        "ok": errors.is_empty(),
        "project": name,
        "type": if wtype.is_empty() { "unknown".to_string() } else { wtype },
        "title": title,
        "entry": entry,
        "errors": errors,
        "warnings": warnings,
    })
}

/// 就地去重、保持首次出现顺序
fn dedupe(v: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|s| seen.insert(s.clone()));
}

/// 网页壁纸入口（与 wallpaper::resolve_item_config 同一优先级）
fn web_entry(dir: &Path, project: &Value) -> Option<String> {
    if let Some(rel) = project.get("file").and_then(|f| f.as_str()) {
        if let Ok(p) = safe_join(dir, rel) {
            if p.is_file() {
                return Some(rel.to_string());
            }
        }
    }
    if dir.join("web/index.html").is_file() {
        return Some("web/index.html".into());
    }
    if dir.join("index.html").is_file() {
        return Some("index.html".into());
    }
    let mut found = Vec::new();
    find_html(dir, dir, &mut found);
    found.sort();
    found.into_iter().next()
}

fn find_html(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // 隐藏目录（`.version/` 的历史副本）不该被当成网页入口
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if is_symlink(&path) {
            continue;
        }
        if path.is_dir() {
            find_html(root, &path, out);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm") {
                out.push(rel_display(root, &path));
            }
        }
    }
}

/// general.properties 校验（WE 用户属性）
fn check_properties(project: &Value, errors: &mut Vec<String>, warnings: &mut Vec<String>) {
    let Some(props) = project
        .get("general")
        .and_then(|g| g.get("properties"))
        .and_then(|p| p.as_object())
    else {
        return;
    };
    for (name, def) in props {
        let Some(def) = def.as_object() else {
            errors.push(format!("属性 {name} 不是对象"));
            continue;
        };
        let Some(ptype) = def.get("type").and_then(|t| t.as_str()) else {
            // 无 type 的是纯显示项（tip / ui_*），WE 不当作属性下发
            continue;
        };
        if ptype.is_empty() {
            continue;
        }
        let ok_type = matches!(
            ptype,
            "color" | "slider" | "checkbox" | "bool" | "combo" | "text" | "textinput"
                | "file" | "directory" | "group"
        );
        if !ok_type {
            errors.push(format!("属性 {name} 的 type 不支持: {ptype}"));
        }
        if let Some(value) = def.get("value") {
            let type_ok = match ptype {
                "color" | "text" | "textinput" | "file" | "directory" => {
                    value.is_string() || is_user_prop_wrapper(value)
                }
                "slider" => value.is_number() || is_user_prop_wrapper(value),
                "checkbox" | "bool" => value.is_boolean() || is_user_prop_wrapper(value),
                _ => true,
            };
            if !type_ok {
                errors.push(format!(
                    "属性 {name} 的 value 与 type={ptype} 不匹配（color/text 用字符串，slider 用数字，checkbox 用布尔）"
                ));
            }
        }
        if ptype == "combo" {
            match def.get("options").and_then(|o| o.as_array()) {
                Some(opts) if !opts.is_empty() => {
                    for (i, o) in opts.iter().enumerate() {
                        if o.get("value").is_none() {
                            errors.push(format!("属性 {name} 的第 {i} 个选项缺少 value"));
                        }
                    }
                }
                _ => errors.push(format!("属性 {name} 是 combo，但没有有效 options")),
            }
        }
        if def.get("order").is_none() {
            warnings.push(format!("属性 {name} 没有 order（面板里会排在最后）"));
        }
    }
}

fn is_user_prop_wrapper(v: &Value) -> bool {
    v.as_object()
        .map(|o| o.contains_key("value") || o.contains_key("user"))
        .unwrap_or(false)
}

/// `unbound_ok_prop` 给体检用：这些属性声明了不绑定也合法（WE 约定）
pub(crate) fn unbound_ok_prop(name: &str) -> bool {
    UNBOUND_OK_PROPS.contains(&name)
}

/// scene.json 校验：校验 v1 明确支持的子集，未知字段一律放过（WE 原生格式）。
///
/// 这里的原则是**「文档写了什么就校验什么」**：agent 拿不到渲染器的逐帧反馈
/// （诊断只保留最后一条，见 content_server），`project_validate` 是它在打包前
/// 唯一能自查的通道。字段级检查漏一项，坏场景就会一路走到「装上桌面才发现
/// 效果静默消失」——比报错难查一个数量级。
fn check_scene(
    dir: &Path,
    project: &Value,
    scene: &Value,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    let general = scene.get("general");
    let ortho = general
        .and_then(|g| g.get("orthogonalprojection"))
        .map(|o| (o.get("width"), o.get("height")));
    let design = match &ortho {
        Some((Some(w), Some(h))) if w.as_f64().unwrap_or(0.0) > 0.0 && h.as_f64().unwrap_or(0.0) > 0.0 => {
            Some((w.as_f64().unwrap_or(0.0), h.as_f64().unwrap_or(0.0)))
        }
        _ => {
            warnings.push(
                "scene.json 的 general.orthogonalprojection 缺 width/height（渲染分辨率会退化成窗口尺寸）".into(),
            );
            None
        }
    };
    if scene.get("camera").is_none() {
        warnings.push("scene.json 没有 camera（视差与 3D 类图层会异常）".into());
    }
    let Some(objects) = scene.get("objects").and_then(|o| o.as_array()) else {
        errors.push("scene.json 缺少 objects 数组".into());
        return;
    };
    if objects.is_empty() {
        warnings.push("scene.json 的 objects 为空（场景什么都没有）".into());
    }

    // 用户属性的**定义**在 project.json，绑定写在 scene.json 的任意字段上
    // （`{"value": …, "user": "<属性名>"}`）。名字对不上时画面毫无反应、且没有任何
    // 报错 —— 这是场景创作里最常见的「改了属性没用」。
    let props = project
        .get("general")
        .and_then(|g| g.get("properties"))
        .and_then(|p| p.as_object())
        .cloned()
        .unwrap_or_default();
    let mut used_props: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    // 父子关系单独收一遍：parent 可以指向后面才出现的图层
    let mut parents: Vec<(i64, i64, String)> = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        let label = obj
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("#{i}"));
        if let Some(id) = obj.get("id").and_then(|v| v.as_i64()) {
            if !ids.insert(id) {
                errors.push(format!("objects 里 id={id} 重复（{label}）"));
            }
        } else {
            warnings.push(format!("对象 {label} 缺少 id"));
        }
        for field in ["origin", "scale", "angles"] {
            if let Some(v) = obj.get(field) {
                if parse_triple(v).is_none() {
                    errors.push(format!(
                        "对象 {label} 的 {field} 不是 \"x y z\" 三元组（受属性控制时用 {{\"value\":\"x y z\"}} 包装）"
                    ));
                }
            }
        }
        if let Some(sz) = obj.get("size") {
            if parse_pair(sz).is_none() {
                errors.push(format!("对象 {label} 的 size 不是 \"w h\" 二元组"));
            }
        }
        let has_image = obj.get("image").and_then(|v| v.as_str());
        let has_particle = obj.get("particle").and_then(|v| v.as_str());
        if has_image.is_some() && has_particle.is_some() {
            warnings.push(format!(
                "对象 {label} 同时写了 image 与 particle（二选一；渲染器只会用其中一个）"
            ));
        }
        if let Some(rel) = has_image {
            check_model_chain(dir, rel, &label, errors, warnings);
        }
        if let Some(rel) = has_particle {
            check_particle_layer(dir, rel, &label, errors, warnings);
        }
        // parent 指向不存在的 id：本层的 origin/scale/angles 会落在错误的坐标系里，
        // 表现是「图层位置莫名其妙」，肉眼极难定位
        if let Some(pid) = obj.get("parent").and_then(|v| v.as_i64()) {
            if let Some(id) = obj.get("id").and_then(|v| v.as_i64()) {
                parents.push((id, pid, label.clone()));
            } else {
                warnings.push(format!("对象 {label} 有 parent，但自己没有 id（无法被其它层引用）"));
            }
        }
        if let Some(v) = obj.get("visible") {
            if !v.is_boolean() && !is_user_prop_wrapper(v) {
                errors.push(format!("对象 {label} 的 visible 不是布尔（受属性控制时用 {{\"value\":true}} 包装）"));
            }
        } else {
            warnings.push(format!("对象 {label} 没有 visible（默认按可见处理）"));
        }
        check_layer_numbers(obj, &label, design, warnings);
        // effects：效果链的每一环都要在工程里真实存在
        if let Some(fx) = obj.get("effects") {
            match fx.as_array() {
                Some(list) => {
                    for (fi, e) in list.iter().enumerate() {
                        let name = e
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("#{fi}"));
                        match e.get("file").and_then(|f| f.as_str()) {
                            Some(rel) => check_effect_file(dir, rel, &label, &name, errors, warnings),
                            None => errors.push(format!(
                                "对象 {label} 的第 {fi} 个 effects 项缺少 file（形如 effects/xxx.json）"
                            )),
                        }
                    }
                }
                None => errors.push(format!("对象 {label} 的 effects 不是数组")),
            }
        }
        // 字段里的属性绑定与关键帧动画：任何一层都可能绑
        let mut walk = SceneWalk {
            props: &props,
            used: &mut used_props,
            errors,
            warnings,
        };
        walk.visit(obj, &format!("对象 {label}"), 0);
    }

    check_parent_graph(&ids, &parents, errors);
    check_unused_properties(&props, &used_props, warnings);
}

/// 单层的数值合理性（超出范围不会报错、只会「看着不对」，所以一律 warning）
fn check_layer_numbers(
    obj: &Value,
    label: &str,
    design: Option<(f64, f64)>,
    warnings: &mut Vec<String>,
) {
    for field in ["alpha", "brightness"] {
        let Some(v) = obj.get(field) else { continue };
        let Some(n) = user_prop_number(v) else { continue };
        if !(0.0..=1.0).contains(&n) {
            warnings.push(format!(
                "对象 {label} 的 {field}={n} 超出 0..1（渲染器按钳制后的值用）"
            ));
        }
    }
    if let Some(sc) = obj.get("scale").and_then(parse_triple) {
        if sc[0].abs() < f64::EPSILON || sc[1].abs() < f64::EPSILON {
            warnings.push(format!(
                "对象 {label} 的 scale={} 有 0 分量（该方向会被压成 0 像素，看不见）",
                sc.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" ")
            ));
        }
    }
    if let Some(sz) = obj.get("size").and_then(parse_pair) {
        if sz[0] <= 0.0 || sz[1] <= 0.0 {
            warnings.push(format!(
                "对象 {label} 的 size=\"{} {}\" 不是正数（该层不会显示）",
                sz[0], sz[1]
            ));
        }
    }
    // 画面外的图层：agent 最常见的坐标系误用就是「origin.y 当成从上往下」
    if let (Some((dw, dh)), Some(o)) = (design, obj.get("origin").and_then(parse_triple)) {
        let margin = 2.0; // 允许 2 像素的边界误差，避免贴边层误报
        if o[0] < -margin || o[1] < -margin || o[0] > dw + margin || o[1] > dh + margin {
            warnings.push(format!(
                "对象 {label} 的 origin=\"{} {} {}\" 落在设计分辨率 {dw}×{dh} 之外（坐标原点在左下角、Y 轴向上；画面正中是 \"{} {}\"）",
                o[0],
                o[1],
                o[2],
                dw / 2.0,
                dh / 2.0
            ));
        }
    }
    if let Some(blend) = obj.get("colorBlendMode").and_then(|v| v.as_i64()) {
        // 文档里点名的几种；其余值渲染库走「正常混合」兜底，静默降级比报错更难查
        if !matches!(blend, 0 | 2 | 6 | 7 | 9 | 31) {
            warnings.push(format!(
                "对象 {label} 的 colorBlendMode={blend} 不在文档列出的 0/2/6/7/9/31 里（多半按正常混合处理）"
            ));
        }
    }
}

/// 从字面值或属性包装里取出数字
fn user_prop_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Object(_) => v.get("value").and_then(|x| x.as_f64()),
        _ => None,
    }
}

/// 遍历一层里的所有字段，做两件事：
///   ① `{"user": "属性名"}` 的绑定名必须在 project.json 的 general.properties 里存在；
///   ② `{"animation": …}` 的关键帧结构要能播（缺 c0 / fps 非法等在这里报出来）。
///
/// 递归深度设上限：JSON 来自 agent，畸形嵌套不该把校验器带崩。
struct SceneWalk<'a> {
    props: &'a serde_json::Map<String, Value>,
    used: &'a mut std::collections::HashSet<String>,
    errors: &'a mut Vec<String>,
    warnings: &'a mut Vec<String>,
}

impl SceneWalk<'_> {
    fn visit(&mut self, node: &Value, path: &str, depth: usize) {
        if depth > 12 {
            return;
        }
        match node {
            Value::Object(map) => {
                if let Some(u) = map.get("user").and_then(|v| v.as_str()) {
                    if u.is_empty() {
                        self.errors
                            .push(format!("{path} 的 user 绑定是空字符串"));
                    } else if self.props.contains_key(u) {
                        self.used.insert(u.to_string());
                    } else {
                        let mut names: Vec<&str> =
                            self.props.keys().map(|k| k.as_str()).collect();
                        names.sort_unstable();
                        let hint = if names.is_empty() {
                            "project.json 的 general.properties 里一个属性都没有".to_string()
                        } else {
                            format!("已声明的属性：{}", names.join(" / "))
                        };
                        self.errors.push(format!(
                            "{path} 绑定了属性「{u}」，但 project.json 里没有这个属性（{hint}）"
                        ));
                    }
                }
                if let Some(anim) = map.get("animation") {
                    check_animation(anim, path, self.errors, self.warnings);
                }
                // 注：angles 动画曾会让首帧永不完成（webwallgl#9），已在渲染库提交 38e877c
                // 修掉（实测同一条场景现在正常 ready）。校验器**不再对此告警** —— 常驻警报
                // 会变成噪音，而且它无法知道调用方链接的是哪一版库。
                // 「链接到旧 dist」这个坑记在 wallpaperem://reference/pitfalls 里。
                for (k, v) in map {
                    self.visit(v, &format!("{path}.{k}"), depth + 1);
                }
            }
            Value::Array(arr) => {
                for (i, v) in arr.iter().enumerate() {
                    self.visit(v, &format!("{path}[{i}]"), depth + 1);
                }
            }
            _ => {}
        }
    }
}

/// 关键帧动画结构校验（文档 §7）
fn check_animation(
    anim: &Value,
    path: &str,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    let Some(obj) = anim.as_object() else {
        errors.push(format!("{path} 的 animation 不是对象"));
        return;
    };
    match obj.get("c0").and_then(|c| c.as_array()) {
        Some(frames) if !frames.is_empty() => {
            let mut max_frame = 0.0f64;
            for (i, f) in frames.iter().enumerate() {
                match f.get("frame").and_then(|v| v.as_f64()) {
                    Some(fr) => max_frame = max_frame.max(fr),
                    None => errors.push(format!(
                        "{path} 的 animation.c0[{i}] 缺少数字 frame（形如 {{\"frame\":0,\"value\":…}}）"
                    )),
                }
                if f.get("value").is_none() {
                    errors.push(format!("{path} 的 animation.c0[{i}] 缺少 value"));
                }
                if let Some(front) = f.get("front") {
                    if front.get("x").and_then(|v| v.as_f64()).is_none() {
                        warnings.push(format!(
                            "{path} 的 animation.c0[{i}].front 没有数字 x（贝塞尔手柄不生效）"
                        ));
                    }
                }
            }
            if let Some(opts) = obj.get("options") {
                match opts.get("fps").and_then(|v| v.as_f64()) {
                    Some(fps) if fps > 0.0 => {}
                    Some(fps) => errors.push(format!("{path} 的 animation.options.fps={fps} 必须大于 0")),
                    None => warnings.push(format!("{path} 的 animation.options 没有 fps（按 30 处理）")),
                }
                match opts.get("length").and_then(|v| v.as_f64()) {
                    Some(len) if len >= 1.0 => {
                        if max_frame > len {
                            warnings.push(format!(
                                "{path} 的关键帧到第 {max_frame} 帧，但 options.length={len} —— 超出部分不会播"
                            ));
                        }
                    }
                    Some(len) => errors.push(format!("{path} 的 animation.options.length={len} 必须 ≥ 1")),
                    None => {}
                }
                if let Some(mode) = opts.get("mode").and_then(|v| v.as_str()) {
                    if !matches!(mode, "loop" | "mirror" | "single") {
                        warnings.push(format!(
                            "{path} 的 animation.options.mode=\"{mode}\" 不是 loop/mirror/single 之一"
                        ));
                    }
                }
            } else {
                warnings.push(format!("{path} 的 animation 没有 options（fps/length/mode 全按默认）"));
            }
        }
        Some(_) => errors.push(format!("{path} 的 animation.c0 是空数组（至少要有两个关键帧）")),
        None => errors.push(format!(
            "{path} 有 animation 但没有 c0 关键帧通道（v1 只支持 c0）"
        )),
    }
}

/// 粒子预设校验（文档 §8）：预设要能解析、材质必须是 genericparticle、
/// 组件名要在实现白名单里（不在的实现里会被**静默忽略**）。  
fn check_particle_layer(
    dir: &Path,
    rel: &str,
    label: &str,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    let Ok(preset_path) = safe_join(dir, rel) else {
        errors.push(format!("对象 {label} 的 particle 路径非法: {rel}"));
        return;
    };
    if !preset_path.is_file() {
        errors.push(format!("对象 {label} 引用的粒子预设不存在: {rel}"));
        return;
    }
    let Some(preset) = std::fs::read_to_string(&preset_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        errors.push(format!("粒子预设不是合法 JSON: {rel}"));
        return;
    };
    // 材质链：粒子材质的第一个 pass 必须是 genericparticle，否则渲染器不画粒子
    match preset.get("material").and_then(|m| m.as_str()) {
        Some(mat_rel) => {
            let passes = safe_join(dir, mat_rel)
                .ok()
                .filter(|p| p.is_file())
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|m| m.get("passes").and_then(|p| p.as_array()).cloned());
            match passes {
                Some(ps) => {
                    let shader = ps
                        .first()
                        .and_then(|p| p.get("shader"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    if shader != "genericparticle" {
                        errors.push(format!(
                            "粒子预设 {rel} 的材质 {mat_rel} 第一个 pass 的 shader 是 \"{shader}\"，必须是 genericparticle"
                        ));
                    }
                }
                None => errors.push(format!(
                    "粒子预设 {rel} 的材质不存在或没有 passes: {mat_rel}"
                )),
            }
        }
        None => errors.push(format!("粒子预设 {rel} 缺少 material 字段")),
    }
    // 发射器：库只特判 boxrandom，其余按球壳发射 —— 名字不认识不等于不生效，
    // 所以这里只查「有没有写 name」（漏写会被当成空名字，那才是真不生效）
    if let Some(items) = preset.get("emitter").and_then(|v| v.as_array()) {
        for (ei, it) in items.iter().enumerate() {
            if it.get("name").and_then(|n| n.as_str()).unwrap_or("").is_empty() {
                warnings.push(format!(
                    "粒子预设 {rel} 的第 {ei} 个 emitter 没有 name（boxrandom 之外都按球壳发射，缺名字等于发射器无效）"
                ));
            }
        }
    }
    // initializer / operator / renderer：名字必须在库的 switch 分支里，否则被静默忽略
    for (key, supported) in [
        ("initializer", PARTICLE_INITIALIZERS.as_slice()),
        ("operator", PARTICLE_OPERATORS.as_slice()),
        ("renderer", PARTICLE_RENDERERS.as_slice()),
    ] {
        let Some(items) = preset.get(key).and_then(|v| v.as_array()) else {
            if key == "renderer" {
                errors.push(format!("粒子预设 {rel} 缺少 renderer（至少要一个 sprite）"));
            }
            continue;
        };
        for it in items {
            let Some(name) = it.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let lname = name.to_ascii_lowercase();
            if !supported.iter().any(|s| *s == lname) {
                warnings.push(format!(
                    "粒子预设 {rel} 的 {key}「{name}」不受支持，会被忽略；支持：{}",
                    supported.join(" / ")
                ));
            }
        }
    }
    if preset.get("controlpoint").and_then(|v| v.as_array()).is_none() {
        warnings.push(format!(
            "粒子预设 {rel} 没有 controlpoint 数组（模板里那份是 8 项，照抄最稳）"
        ));
    }
}

/// 效果文件：存在 + 里面用到的 shader 是否有源码（没有源码的 pass 会被静默跳过）
fn check_effect_file(
    dir: &Path,
    rel: &str,
    label: &str,
    name: &str,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    let Ok(path) = safe_join(dir, rel) else {
        errors.push(format!("对象 {label} 的效果 {name} 路径非法: {rel}"));
        return;
    };
    if !path.is_file() {
        errors.push(format!(
            "对象 {label} 的效果「{name}」找不到文件: {rel}（effects/<名字>.json）"
        ));
        return;
    }
    let Some(effect) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        errors.push(format!("效果文件不是合法 JSON: {rel}"));
        return;
    };
    let Some(passes) = effect.get("passes").and_then(|p| p.as_array()) else {
        errors.push(format!("效果文件 {rel} 缺少 passes 数组"));
        return;
    };
    for (pi, pass) in passes.iter().enumerate() {
        // 三种合法形态（WE 官方效果文件三者都有）：
        //   ① `{"material":"materials/effects/x.json"}` —— **官方主流写法**，shader 在材质里
        //   ② `{"shader":"xxx"}` —— 直写 shader（简写/内联）
        //   ③ `{"command":"copy"|"swap", ...}` —— 命令 pass，本就没有 shader
        if let Some(shader) = pass.get("shader").and_then(|s| s.as_str()) {
            // 效果文件里直写 shader **不会被渲染库执行**：resolveEffectChain 只认
            // `passes[].material`，没有 material 又没有 command 的 pass 被当成"整块拷贝"
            // 命令 pass（shader=null）→ 静默什么都不画，而且没有任何日志（webwallgl#11）。
            // 语法合法、也照着材质 JSON 的形状写得很像，所以必须这里点破。
            warnings.push(format!(
                "效果 {rel} 第 {pi} 个 pass 直写了 shader「{shader}」—— 渲染库在**效果文件里只认 material**，\
                 这一趟会被当成整块拷贝的命令 pass 静默丢弃（图层只会显示内置材质的底色）。\
                 改法：effects/{name}.json 写 material，shader 写到 materials/effects/*.json 里"
            ));
            check_shader_present(dir, shader, rel, pi, warnings);
            continue;
        }
        if pass.get("command").is_some() {
            continue;
        }
        let Some(material) = pass.get("material").and_then(|m| m.as_str()) else {
            errors.push(format!(
                "效果 {rel} 第 {pi} 个 pass 既没有 material / shader，也不是 command（copy/swap）"
            ));
            continue;
        };
        // 材质文件必须在工程里（效果链只从 pkg 里取，不认 WE 安装目录的效果材质）
        let Ok(mpath) = safe_join(dir, material) else {
            errors.push(format!("效果 {rel} 第 {pi} 个 pass 的 material 路径非法: {material}"));
            continue;
        };
        if !mpath.is_file() {
            errors.push(format!(
                "效果 {rel} 第 {pi} 个 pass 引用的材质不存在: {material}"
            ));
            continue;
        }
        match std::fs::read_to_string(&mpath)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        {
            Some(mj) => match mj
                .get("passes")
                .and_then(|p| p.as_array())
                .and_then(|a| a.first())
                .and_then(|p| p.get("shader"))
                .and_then(|s| s.as_str())
            {
                Some(shader) => check_shader_present(dir, shader, material, 0, warnings),
                None => errors.push(format!(
                    "效果 {rel} 第 {pi} 个 pass 的材质 {material} 第一个 pass 没有 shader"
                )),
            },
            None => errors.push(format!("效果材质 {material} 不是合法 JSON")),
        }
    }
}

/// 效果里某一趟用的 shader 是否可解析：内置的直通，其余必须在工程里有
/// `shaders/<名字>.frag`（或 `.vert`），否则渲染器会跳过这一趟 —— 只给 warning。
fn check_shader_present(
    dir: &Path,
    shader: &str,
    where_: &str,
    pi: usize,
    warnings: &mut Vec<String>,
) {
    if is_builtin_shader(shader) {
        return;
    }
    let frag = dir.join(format!("shaders/{shader}.frag")).is_file();
    let vert = dir.join(format!("shaders/{shader}.vert")).is_file();
    if !frag && !vert {
        warnings.push(format!(
            "效果 {where_} 第 {pi} 个 pass 用了 {shader}，工程里没有 shaders/{shader}.frag/.vert（渲染器会跳过该 pass）"
        ));
        return;
    }
    // ⚠️ 只给 .frag 不给 .vert 是**静默失败**：顶点着色器取不到 → 链接失败 → 整趟
    // 被跳过，而且只在 console.warn 里说一句，宿主诊断通道看不到；画面表现是
    // 「图层退回内置材质画出的纯色块」。这条必须主动点出来（踩过，见 webwallgl#10）。
    if frag && !vert {
        warnings.push(format!(
            "效果 {where_} 第 {pi} 个 pass 的 shader「{shader}」只有 .frag、缺 shaders/{shader}.vert —— \
             渲染库会**静默跳过**这一趟（图层只剩内置材质的纯色块，诊断通道也没有记录）。\
             顶点着色器照抄官方效果那份 passthrough 即可（见资源 wallpaperem://reference/effects）"
        ));
    }
    if vert && !frag {
        warnings.push(format!(
            "效果 {where_} 第 {pi} 个 pass 的 shader「{shader}」只有 .vert、缺 shaders/{shader}.frag"
        ));
    }
}

/// parent 指向存在的 id、且不成环
fn check_parent_graph(
    ids: &std::collections::HashSet<i64>,
    parents: &[(i64, i64, String)],
    errors: &mut Vec<String>,
) {
    for (id, pid, label) in parents {
        if !ids.contains(pid) {
            errors.push(format!(
                "对象 {label} 的 parent={pid} 指向不存在的图层 id（可用 id：{}）",
                {
                    let mut v: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
                    v.sort_unstable();
                    if v.is_empty() { "（没有任何图层带 id）".to_string() } else { v.join(" / ") }
                }
            ));
        } else if *id == *pid {
            errors.push(format!("对象 {label} 的 parent 指向自己（id={id}）"));
        }
    }
    // 成环检测：沿 parent 一路向上，步数上限 = 图层数（超过必然成环）
    let map: std::collections::HashMap<i64, i64> =
        parents.iter().map(|(id, pid, _)| (*id, *pid)).collect();
    for (start, _, label) in parents {
        let mut cur = *start;
        let mut steps = 0usize;
        while let Some(next) = map.get(&cur) {
            cur = *next;
            steps += 1;
            if cur == *start {
                errors.push(format!("对象 {label} 的 parent 链成环（id={start}）"));
                break;
            }
            if steps > map.len() + 1 {
                break;
            }
        }
    }
}

/// 声明了但没有任何图层绑定的属性：多半是「想给用户调，但忘了绑」
fn check_unused_properties(
    props: &serde_json::Map<String, Value>,
    used: &std::collections::HashSet<String>,
    warnings: &mut Vec<String>,
) {
    let mut unused: Vec<&str> = props
        .iter()
        // 纯显示项（tip / 分节标题）本来就不需要绑定
        .filter(|(_, v)| v.get("type").and_then(|t| t.as_str()).is_some())
        .map(|(k, _)| k.as_str())
        // WE 约定里由工坊/浏览页消费的属性（浏览页配色），不需要图层绑定
        .filter(|k| !UNBOUND_OK_PROPS.contains(k))
        .filter(|k| !used.contains(*k))
        .collect();
    if unused.is_empty() {
        return;
    }
    unused.sort_unstable();
    warnings.push(format!(
        "这些属性声明了但没有任何图层绑定（scene.json 里用 {{\"value\":…,\"user\":\"属性名\"}} 绑定）：{}",
        unused.join(" / ")
    ));
}


/// image → models/x.json → materials/y.json → passes → textures 全链路校验
fn check_model_chain(
    dir: &Path,
    model_rel: &str,
    label: &str,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    // WE 内置模型（纯色层/效果画布）不在工程里，允许直接引用
    if model_rel.starts_with("models/util/") {
        return;
    }
    let Ok(model_path) = safe_join(dir, model_rel) else {
        errors.push(format!("对象 {label} 的 image 路径非法: {model_rel}"));
        return;
    };
    let Some(model) = std::fs::read_to_string(&model_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        errors.push(format!("对象 {label} 引用的模型不存在或不是合法 JSON: {model_rel}"));
        return;
    };
    let Some(mat_rel) = model.get("material").and_then(|m| m.as_str()) else {
        errors.push(format!("模型 {model_rel} 没有 material 字段"));
        return;
    };
    let Ok(mat_path) = safe_join(dir, mat_rel) else {
        errors.push(format!("模型 {model_rel} 的 material 路径非法: {mat_rel}"));
        return;
    };
    let Some(mat) = std::fs::read_to_string(&mat_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        errors.push(format!("模型 {model_rel} 引用的材质不存在或非法: {mat_rel}"));
        return;
    };
    let Some(passes) = mat.get("passes").and_then(|p| p.as_array()) else {
        errors.push(format!("材质 {mat_rel} 缺少 passes 数组"));
        return;
    };
    for (pi, pass) in passes.iter().enumerate() {
        if let Some(shader) = pass.get("shader").and_then(|s| s.as_str()) {
            if !is_builtin_shader(shader) && !dir.join(format!("shaders/{shader}.frag")).is_file() {
                warnings.push(format!(
                    "材质 {mat_rel} 第 {pi} 个 pass 用了 {shader}，但工程里没有 shaders/{shader}.frag/.vert（渲染器会跳过该 pass）"
                ));
            }
        } else {
            errors.push(format!("材质 {mat_rel} 第 {pi} 个 pass 缺少 shader"));
        }
        if let Some(textures) = pass.get("textures").and_then(|t| t.as_array()) {
            for tex in textures {
                let Some(name) = tex.as_str() else { continue };
                if name.is_empty()
                    || name.starts_with("util/")
                    || name.starts_with("particle/")
                    || name.starts_with("_rt_")
                {
                    continue;
                }
                let tex_rel = format!("materials/{name}.tex");
                let has_tex = dir.join(&tex_rel).is_file();
                let src_exts = ["png", "jpg", "jpeg"];
                let src = src_exts
                    .iter()
                    .find(|ext| dir.join(format!("materials/{name}.{ext}")).is_file())
                    .copied();
                if !has_tex && src.is_none() {
                    errors.push(format!(
                        "材质 {mat_rel} 引用的贴图缺失：需要 materials/{name}.tex 或 materials/{name}.png"
                    ));
                } else if has_tex && src.is_some() {
                    // 同名 .tex 与源图并存时，打包器「.tex 优先」会**静默丢掉源图** ——
                    // 表现是「改了贴图但画面没变」。这里直接拦下来，让 agent 明确删一份。
                    errors.push(format!(
                        "贴图同名冲突：materials/{name}.tex 与 materials/{name}.{} 同时存在。\
                         scene_pack 会优先用 .tex（源图被忽略），画面会和预期不一致 —— \
                         请删掉其中一份（一般删 .tex，让 scene_pack 从源图转）",
                        src.unwrap()
                    ));
                } else if let Some(ext) = src {
                    // 只有源图：能不能解码要在这里就报错。等 scene_pack 才失败的话，
                    // agent 拿到的只是「转换失败」，看不出是源图本身坏了。
                    if let Err(e) = image_source_dims(dir, name) {
                        errors.push(format!(
                            "贴图 materials/{name}.{ext} 无法解码（{e}）；scene_pack 转不出 .tex"
                        ));
                    }
                }
            }
        }
    }
}

/// 读 materials/<name>.(png|jpg|jpeg) 的尺寸，顺带验证它确实是张能解出来的图。
/// 返回 (相对路径, 宽, 高)。
pub(crate) fn image_source_dims(dir: &Path, name: &str) -> Result<(String, u32, u32), String> {
    for ext in ["png", "jpg", "jpeg"] {
        let rel = format!("materials/{name}.{ext}");
        let path = dir.join(&rel);
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("读取失败: {e}"))?;
        let dims = match ext {
            "png" => png_dims(&bytes)?,
            _ => jpeg_dims(&bytes)?,
        };
        return Ok((rel, dims.0, dims.1));
    }
    Err("找不到源图".into())
}

/// WE 内置反照率直通 shader（渲染库用 copy pass 画，不需要 pkg 内嵌源码）。
///
/// ⚠️ 这是渲染库那份名单的**镜像**：库侧新增/改名时必须同步这里，
/// `scene_support_matches_library_bundle` 测试会用 bundle 里的字面量兜一道。
pub(crate) fn is_builtin_shader(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(n.as_str(), "generic" | "sprite" | "flat" | "genericparticle")
        || n.starts_with("genericimage")
}

/// 内置模型（工程里不用带，`project_validate` 跳过它们的引用链检查）
pub(crate) const BUILTIN_MODELS: [&str; 4] = [
    "models/util/solidlayer.json",
    "models/util/composelayer.json",
    "models/util/projectlayer.json",
    "models/util/fullscreenlayer.json",
];

/// 贴图名里带这些词会被解析成对应的内置粒子形状（库侧按名字启发式识别）
pub(crate) const PARTICLE_TEXTURE_KEYWORDS: [&str; 7] =
    ["star", "snow", "rain", "smoke", "fire", "leaf", "flare"];

/// 场景能力清单（渲染库 v1 支持范围的真源，供 MCP 资源与校验器共用）。
///
/// 目的：agent 不必从散文文档里猜「哪些名字会被静默忽略」。校验器拿它报 warning，
/// `wallpaperem://reference/scene-support` 资源把它整个吐给客户端。
pub(crate) fn scene_support_json() -> Value {
    json!({
        "shaders": {
            "builtin": ["generic", "genericimage", "genericimage2", "sprite", "flat", "genericparticle"],
            "note": "其余名字必须在工程里带 shaders/<名字>.frag（可选 .vert），否则该 pass 被跳过",
        },
        "models": { "builtin": BUILTIN_MODELS },
        "textures": {
            "project": "materials/<名字>.(png|jpg|jpeg) 由 scene_pack 转成同名 .tex 打进包；同名 .tex 与源图并存是 error",
            "builtin": "util/<名字>（内置贴图）、_rt_<名字>（效果链渲染目标）",
            "particle": format!("particle/<名字>（程序化重建，名字里带 {} 之一会被识别成对应形状）", PARTICLE_TEXTURE_KEYWORDS.join("/")),
            "rejected": ["webp"],
        },
        "blending": ["normal", "translucent", "additive"],
        "particles": {
            "emitter": PARTICLE_EMITTERS,
            "emitterNote": "只有 boxrandom 被特判，其余任何名字都按球壳发射（WE 里叫 sphererandom）",
            "initializer": PARTICLE_INITIALIZERS,
            "operator": PARTICLE_OPERATORS,
            "renderer": PARTICLE_RENDERERS,
            "material_shader": "genericparticle",
            "note": "名单外的名字会被渲染库静默忽略，project_validate 会对逐个名字给 warning",
        },
        "animation": { "channels": ["c0"], "modes": ["loop", "mirror", "single"], "defaultFps": 30 },
        "userProps": {
            "types": ["color", "slider", "checkbox", "bool", "combo", "text", "textinput", "file", "directory", "group"],
            "binding": "scene.json 任意字段写成 {\"value\": <默认值>, \"user\": \"<属性名>\"}；属性名必须在 project.json 的 general.properties 里",
        },
        "limits": {
            "writeFileBytes": MAX_WRITE_BYTES,
            "packBytes": MAX_PACK_BYTES,
            "requestBodyBytes": 64 << 20,
        },
    })
}

pub(crate) fn parse_triple(v: &Value) -> Option<[f64; 3]> {
    parse_floats(v, 3).map(|mut f| {
        let mut out = [0.0; 3];
        out[..3].copy_from_slice(&f.drain(..3).collect::<Vec<_>>());
        out
    })
}

pub(crate) fn parse_pair(v: &Value) -> Option<[f64; 2]> {
    parse_floats(v, 2).map(|mut f| {
        let mut out = [0.0; 2];
        out[..2].copy_from_slice(&f.drain(..2).collect::<Vec<_>>());
        out
    })
}

/// 接受 "a b c" 字符串或 `{"value":"a b c"}`（属性包装）两种形态
fn parse_floats(v: &Value, n: usize) -> Option<Vec<f64>> {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|x| x.as_f64().unwrap_or(f64::NAN).to_string())
            .collect::<Vec<_>>()
            .join(" "),
        Value::Object(_) => return parse_floats(v.get("value")?, n),
        _ => return None,
    };
    let parts: Vec<f64> = s
        .split_whitespace()
        .map(|p| p.parse::<f64>())
        .collect::<Result<_, _>>()
        .ok()?;
    if parts.len() < n {
        return None;
    }
    Some(parts)
}

// ---------------------------------------------------------------- scene.pkg 打包

/// 把工程打成 `scene.pkg`：`materials/**` 下的图片就地转 `.tex`，其余文件原样进包。
///
/// `install=true` 时紧接着装/更新进本地库（省掉 agent 的一次往返：改完 → pack →
/// 装 => 一步）。已装过（库里已有同 id 目录）走**增量更新**，没装过走正常安装。
pub fn pack_scene(app: &AppHandle, name: &str, install: bool) -> Result<Value, String> {
    let dir = project_dir(app, name)?;
    if !dir.is_dir() {
        return Err(format!("工程不存在: {}", name.trim()));
    }
    let mut packed = pack_dir(&dir, name.trim())?;
    if !install {
        return Ok(packed);
    }
    let installed = if installed_item_id(app, name.trim()).is_some() {
        update_project(app, name.trim(), None)?
    } else {
        let (item_id, title, wtype) = install_project(app, name.trim())?;
        json!({ "itemId": item_id, "title": title, "type": wtype, "installed": true })
    };
    if let Some(obj) = packed.as_object_mut() {
        obj.insert("installed".into(), installed);
        obj.insert(
            "hint".into(),
            json!("已装进本地库，可直接 wallpaper_screenshot（会自动重挂到新版）"),
        );
    }
    Ok(packed)
}

/// 打包主体（不碰 AppHandle，便于直接对目录单测整条链路）
fn pack_dir(dir: &Path, name: &str) -> Result<Value, String> {
    // 先校验：有错就别产出包，让 agent 拿到明确反馈
    let report = validate_dir(dir, name);
    let errors: Vec<String> = report
        .get("errors")
        .and_then(|e| e.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("工程校验未通过，先修好再打包：\n- {}", errors.join("\n- ")));
    }
    let wtype = report
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    if wtype != "scene" {
        return Err(format!(
            "scene_pack 只适用于 type=scene 的工程（当前 type={wtype}）；网页壁纸不需要打包"
        ));
    }

    let mut raw: Vec<(String, PathBuf)> = Vec::new();
    let mut total: u64 = 0;
    collect_pack_candidates(dir, dir, &mut raw, &mut total)?;
    if total > MAX_PACK_BYTES {
        return Err(format!(
            "工程过大（{} 字节 > 上限 {MAX_PACK_BYTES}），请把素材压小",
            total
        ));
    }

    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut converted = Vec::new();
    for (rel, path) in raw {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let bytes = std::fs::read(&path).map_err(|e| format!("读取 {rel} 失败: {e}"))?;
        if rel.starts_with("materials/") && matches!(ext.as_str(), "png" | "jpg" | "jpeg") {
            let tex_rel = replace_ext(&rel, "tex");
            // 同名 .tex 与源图并存 → 只能二选一。旧实现按 read_dir 的无序结果
            // 「先遍历到谁就用谁」，同一工程可能这次静默用旧 .tex、下次报「打包条目
            // 重名」。校验器已经拦过一道，这里再兜一次底（pack_dir 也被单测直接调用）。
            if dir.join(&tex_rel).is_file() {
                return Err(format!(
                    "贴图同名冲突：{rel} 与 {tex_rel} 同时存在；请删掉其中一份再打包"
                ));
            }
            let tex = tex_from_image(&bytes, &ext)
                .map_err(|e| format!("贴图 {rel} 转换失败: {e}"))?;
            converted.push(tex_rel.clone());
            entries.push((tex_rel, tex));
            continue;
        }
        if rel.starts_with("materials/") && ext == "webp" {
            return Err(format!(
                "materials 下的 WebP 不受支持（渲染库会把 WebP 当视频纹理）：{rel}，请转成 PNG"
            ));
        }
        entries.push((rel, bytes));
    }
    if entries.is_empty() {
        return Err("工程里没有任何可打包的文件".into());
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for i in 1..entries.len() {
        if entries[i].0 == entries[i - 1].0 {
            return Err(format!("打包条目重名: {}", entries[i].0));
        }
    }

    let pkg = build_pkg(&entries);
    let out = dir.join("scene.pkg");
    std::fs::write(&out, &pkg).map_err(|e| format!("写入 scene.pkg 失败: {e}"))?;
    Ok(json!({
        "project": name,
        "pkg": out.to_string_lossy(),
        "bytes": pkg.len(),
        "entries": entries.iter().map(|(n, b)| json!({ "name": n, "bytes": b.len() })).collect::<Vec<_>>(),
        "convertedTextures": converted,
        "warnings": report.get("warnings").cloned().unwrap_or(json!([])),
    }))
}

/// 打包候选文件：跳过工程元数据、预览图、隐藏项与打包产物
fn collect_pack_candidates(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
    total: &mut u64,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "project.json" || name == "scene.pkg" {
            continue;
        }
        if name.starts_with("preview.") {
            continue;
        }
        if is_symlink(&path) {
            continue;
        }
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        if ft.is_dir() {
            collect_pack_candidates(root, &path, out, total)?;
        } else if ft.is_file() {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            // 顶层文档/脚本不进包：pkg 只装渲染要用的东西
            if ext == "md" || ext == "txt" {
                continue;
            }
            // materials 下的图片源不进包（转成 .tex 后由 .tex 顶替）
            if rel_display(root, &path).starts_with("materials/")
                && matches!(ext.as_str(), "png" | "jpg" | "jpeg")
            {
                *total += entry.metadata().map(|m| m.len()).unwrap_or(0);
                out.push((rel_display(root, &path), path));
                continue;
            }
            *total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push((rel_display(root, &path), path));
        }
    }
    Ok(())
}

/// 打包候选 → 最终条目（图片转 tex）
fn replace_ext(rel: &str, ext: &str) -> String {
    match rel.rsplit_once('.') {
        Some((stem, _)) => format!("{stem}.{ext}"),
        None => format!("{rel}.{ext}"),
    }
}

/// `PKGV00xx` 容器。入口顺序即数据段顺序，offset 相对数据段起点。
fn build_pkg(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut table_len = 4 + PKG_MAGIC.len() + 4;
    for (name, _) in entries {
        table_len += 4 + name.len() + 8;
    }
    let mut out = Vec::with_capacity(table_len + entries.iter().map(|(_, b)| b.len()).sum::<usize>());
    out.extend_from_slice(&(PKG_MAGIC.len() as u32).to_le_bytes());
    out.extend_from_slice(PKG_MAGIC.as_bytes());
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    let mut offset = 0u32;
    for (name, body) in entries {
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        offset += body.len() as u32;
    }
    for (_, body) in entries {
        out.extend_from_slice(body);
    }
    out
}

// ---------------------------------------------------------------- .tex 写出

/// 把 PNG/JPEG 原样封进 `.tex` 容器（freeImage 格式，无需重编码像素）
pub(crate) fn tex_from_image(bytes: &[u8], ext: &str) -> Result<Vec<u8>, String> {
    let ((w, h), fif) = match ext {
        "png" => (png_dims(bytes)?, 13i32),
        "jpg" | "jpeg" => (jpeg_dims(bytes)?, 2i32),
        other => return Err(format!("不支持的图片格式: {other}")),
    };
    if w == 0 || h == 0 {
        return Err("图片尺寸为 0".into());
    }
    let len = bytes.len() as i32;
    let mut out = Vec::with_capacity(bytes.len() + 128);
    out.extend_from_slice(b"TEXV0005\0");
    out.extend_from_slice(b"TEXI0001\0");
    out.extend_from_slice(&0u32.to_le_bytes()); // format：freeImage 时不参与解码
    out.extend_from_slice(&2u32.to_le_bytes()); // flags：对齐 WE 常规取值
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&0xFF00_0000u32.to_le_bytes()); // ignored（编辑器用途）
    out.extend_from_slice(b"TEXB0004\0");
    out.extend_from_slice(&1u32.to_le_bytes()); // imageCount
    out.extend_from_slice(&fif.to_le_bytes()); // freeImageFormat
    out.extend_from_slice(&0u32.to_le_bytes()); // hasMipExtension=0 → 解析方按 V3 布局
    out.extend_from_slice(&1u32.to_le_bytes()); // mipCount
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // compression = 0（不压缩）
    out.extend_from_slice(&len.to_le_bytes()); // uncompressedSize
    out.extend_from_slice(&len.to_le_bytes()); // compressedSize
    out.extend_from_slice(bytes);
    Ok(out)
}

pub(crate) fn png_dims(bytes: &[u8]) -> Result<(u32, u32), String> {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.len() < 24 || bytes[..8] != SIG {
        return Err("不是合法 PNG".into());
    }
    if &bytes[12..16] != b"IHDR" {
        return Err("PNG 缺少 IHDR".into());
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Ok((w, h))
}

/// JPEG 尺寸：扫 SOF 段（不需要解码像素）
pub(crate) fn jpeg_dims(bytes: &[u8]) -> Result<(u32, u32), String> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return Err("不是合法 JPEG".into());
    }
    let mut i = 2usize;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        i += 2;
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if i + 1 >= bytes.len() {
            break;
        }
        let seg_len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
        if seg_len < 2 {
            break;
        }
        // SOF0..SOF15（除 DHT=C4 / JPG=C8 / DAC=CC）里带尺寸
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            if i + 7 >= bytes.len() {
                break;
            }
            let h = u16::from_be_bytes([bytes[i + 3], bytes[i + 4]]) as u32;
            let w = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            return Ok((w, h));
        }
        i += seg_len;
    }
    Err("JPEG 里找不到 SOF 尺寸段".into())
}

// ---------------------------------------------------------------- 安装

/// 把工程安装（导入）进本地库：复用 library 的导入链路（拷贝 + 写库 + 抽封面）。
/// 返回 `(item_id, title, type)`。
pub fn install_project(app: &AppHandle, name: &str) -> Result<(String, String, String), String> {
    let dir = project_dir(app, name)?;
    if !dir.is_dir() {
        return Err(format!("工程不存在: {}", name.trim()));
    }
    let report = validate_dir(&dir, name.trim());
    if report.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let errs: Vec<String> = report
            .get("errors")
            .and_then(|e| e.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        return Err(format!("工程校验未通过：\n- {}", errs.join("\n- ")));
    }
    let tags = read_project_json(&dir)
        .map(|p| project_tags(&p))
        .unwrap_or_default();
    crate::library::import_project_dir(app, &dir, &tags)
}

/// 覆盖安装：把工程目录**增量同步**进库里那份副本，item_id 不变。
///
/// 为什么不再整目录重导：每一轮迭代都要把全部素材（贴图动辄几十上百 MB）重拷一遍，
/// 而迭代里真正变的往往只有 `scene.json` 和 `scene.pkg`。现在先算差异再拷：
///   - 新增 / 大小变了 / 源比库新的文件 → 拷（先写 `.tmp` 再 rename，单文件原子替换）
///   - 源里已删除的文件 → 从库里删掉
///   - 没变的文件一个字节都不动
///
/// 失败时旧副本可能处于「部分更新」状态（不像整目录暂存那样能整体回滚），所以
/// 开头**先校验**——绝大多数失败都是校验不过，那一类根本进不到拷贝阶段。
pub fn update_project(
    app: &AppHandle,
    name: &str,
    item_id: Option<&str>,
) -> Result<Value, String> {
    let target = match item_id.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(id) => id.to_string(),
        None => crate::library::project_item_id_for(name.trim()),
    };
    let dir = project_dir(app, name)?;
    if !dir.is_dir() {
        return Err(format!("工程不存在: {}", name.trim()));
    }
    let report = validate_dir(&dir, name.trim());
    if report.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let errs: Vec<String> = report
            .get("errors")
            .and_then(|e| e.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        return Err(format!("工程校验未通过：\n- {}", errs.join("\n- ")));
    }
    let lib_dir = crate::library::item_dir(app, &target)?;
    if !lib_dir.is_dir() {
        // 没装过（或目录被清掉了）→ 正常安装一次，行为与 project_install 一致
        let (new_id, title, wtype) = install_project(app, name)?;
        return Ok(json!({
            "project": name.trim(),
            "itemId": new_id,
            "previousItemId": target,
            "idChanged": new_id != target,
            "title": title,
            "type": wtype,
            "mode": "install",
        }));
    }

    let stats = sync_dir_filtered(&dir, &lib_dir).map_err(|e| {
        format!("增量同步失败：{e}（库内副本可能处于半更新状态，再跑一次 project_update 即可修复）")
    })?;
    // 标签增量合并（只加不减，保留工坊/用户来源的标签）
    let tags = read_project_json(&dir)
        .map(|p| project_tags(&p))
        .unwrap_or_default();
    if let Err(e) = crate::library::merge_item_tags(app, &target, &tags) {
        tracing::warn!("合并工程标签失败（不影响更新）: {e}");
    }
    crate::library::refresh_item_meta(app, &target, &lib_dir)?;
    let (wtype, title) = read_project_meta(&lib_dir);
    Ok(json!({
        "project": name.trim(),
        "itemId": target,
        "title": title,
        "type": wtype,
        "mode": "incremental",
        "copied": stats.copied,
        "removed": stats.removed,
        "unchanged": stats.unchanged,
        "bytes": stats.bytes,
    }))
}

/// 增量同步的统计（返回给 agent 看「这次到底动了多少东西」）
#[derive(Default, Debug, PartialEq, Eq)]
struct SyncStats {
    copied: u64,
    removed: u64,
    unchanged: u64,
    bytes: u64,
}

/// 把 `from`（工程目录）同步进 `to`（库内副本）：只拷变化的文件、删源里没有的。
///
/// 跳过口径与 `library::copy_dir_filtered` **完全一致**（`.version/` 与符号链接不进库），
/// 否则「源里有、库里不该有」的判定会把版本历史当成待删文件。
/// 单文件走「临时名 + rename」：中途失败最多留一个 `.tmp`，不会把库里的好文件写坏。
fn sync_dir_filtered(from: &Path, to: &Path) -> Result<SyncStats, String> {
    let mut stats = SyncStats::default();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    sync_walk(from, from, to, &mut stats, &mut seen)?;
    // 源里已经删掉的文件 → 库里跟着删（`.tmp` 残渣也顺手清掉）
    let mut stale: Vec<std::path::PathBuf> = Vec::new();
    collect_stale(to, to, &seen, &mut stale)?;
    for p in stale {
        if p.is_dir() {
            let _ = std::fs::remove_dir_all(&p);
        } else if std::fs::remove_file(&p).is_ok() {
            stats.removed += 1;
        }
    }
    Ok(stats)
}

fn sync_walk(
    from_root: &Path,
    from: &Path,
    to: &Path,
    stats: &mut SyncStats,
    seen: &mut std::collections::HashSet<String>,
) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_name().to_string_lossy() == VERSION_DIR {
            continue;
        }
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        if ft.is_symlink() {
            continue;
        }
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let rel = rel_display(from_root, &src);
        if ft.is_dir() {
            sync_walk(from_root, &src, &dst, stats, seen)?;
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        seen.insert(rel.clone());
        let src_len = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let up_to_date = std::fs::metadata(&dst)
            .ok()
            .map(|m| {
                let dst_len = m.len();
                let fresher = m
                    .modified()
                    .ok()
                    .zip(entry.metadata().ok().and_then(|s| s.modified().ok()))
                    .map(|(d, s)| s > d)
                    .unwrap_or(true); // 时间戳拿不到就按「需要拷」处理
                dst_len == src_len && !fresher
            })
            .unwrap_or(false);
        if up_to_date {
            stats.unchanged += 1;
            continue;
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = dst.with_extension(format!(
            "{}.tmp{}",
            dst.extension().and_then(|e| e.to_str()).unwrap_or(""),
            std::process::id()
        ));
        std::fs::copy(&src, &tmp).map_err(|e| format!("拷贝 {rel} 失败: {e}"))?;
        std::fs::rename(&tmp, &dst).map_err(|e| format!("落位 {rel} 失败: {e}"))?;
        stats.copied += 1;
        stats.bytes += src_len;
    }
    Ok(())
}

/// 收集库内副本里「源里已经没有」的文件/目录（`seen` = 源里存在的相对路径）。
///
/// 例外：`preview.*` 只在一处**不是**源文件 —— 视频类条目入库时由导入链路抽首帧生成
/// （见 `library::import_dir_into`）。源工程若本来就没有预览图，照「源里没有就删」会把它
/// 删掉；这里改成「源里有任意 preview 才允许删」。
fn collect_stale(
    to_root: &Path,
    dir: &Path,
    seen: &std::collections::HashSet<String>,
    out: &mut Vec<std::path::PathBuf>,
) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        // 隐藏项（含旧版留下的 `.bak-*`）不归同步管，免得误删备份
        if name.starts_with('.') {
            continue;
        }
        let rel = rel_display(to_root, &path);
        // 源工程里一份预览图都没有时，库内的 preview.* 是导入链路生成的封面，留着
        if name.starts_with("preview.")
            && !seen.iter().any(|s| s.starts_with("preview."))
        {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            collect_stale(to_root, &path, seen, out)?;
            // 目录本身在源里不存在（且已经空了）→ 删掉
            if !seen.iter().any(|s| s.starts_with(&format!("{rel}/"))) && !seen.contains(&rel) {
                out.push(path.clone());
            }
        } else if !seen.contains(&rel) {
            out.push(path.clone());
        }
    }
    Ok(())
}

/// 素材导入的**来源白名单根**与允许的扩展名。
///
/// 为什么要白名单：这条工具是「把宿主上的一个文件拷进工程」，若放开任意绝对路径，
/// MCP 就成了任意文件读取器 —— `~/.ssh/id_rsa` → 拷进工程 → `project_read_file`
/// 读出来。而真实用法（agent 用别的工具生成了贴图/AI 图，落在下载/图片/桌面）
/// 全都在下面这几个目录里。
const ASSET_EXT_ALLOW: [&str; 13] = [
    "png", "jpg", "jpeg", "tex", "frag", "vert", "h", "json", "ogg", "wav", "mp3", "gif", "mp4",
];
/// 素材导入单文件上限（走本地文件系统，不占 JSON-RPC 请求体）
const MAX_ASSET_BYTES: u64 = 512 << 20;

fn asset_roots(app: &AppHandle) -> Vec<PathBuf> {
    let p = app.path();
    [
        p.picture_dir().ok(),
        p.download_dir().ok(),
        p.desktop_dir().ok(),
        p.document_dir().ok(),
        p.audio_dir().ok(),
        p.video_dir().ok(),
        p.temp_dir().ok(),
        p.app_data_dir().ok(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// 校验来源路径：必须在白名单根内、必须是普通文件（不跟随符号链接）、扩展名在白名单里。
/// 返回规范化后的绝对路径。
fn check_asset_source(app: &AppHandle, source: &str) -> Result<PathBuf, String> {
    let raw = PathBuf::from(source.trim());
    if !raw.is_absolute() {
        return Err("source 必须是绝对路径".into());
    }
    let meta = std::fs::symlink_metadata(&raw).map_err(|e| format!("读不到来源文件: {e}"))?;
    if meta.file_type().is_symlink() {
        return Err("来源不能是符号链接（避免绕过目录白名单）".into());
    }
    if !meta.is_file() {
        return Err("source 必须是普通文件".into());
    }
    if meta.len() > MAX_ASSET_BYTES {
        return Err(format!(
            "来源文件过大（{} > 上限 {}）",
            meta.len(),
            MAX_ASSET_BYTES
        ));
    }
    let ext = raw
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !ASSET_EXT_ALLOW.contains(&ext.as_str()) {
        return Err(format!(
            "不允许导入 .{ext}（可用：{}）；这样做是为了不让 MCP 变成任意文件读取器",
            ASSET_EXT_ALLOW.join(" / ")
        ));
    }
    let real = std::fs::canonicalize(&raw).map_err(|e| format!("解析来源路径失败: {e}"))?;
    let in_projects = projects_root(app)
        .ok()
        .and_then(|r| std::fs::canonicalize(r).ok())
        .map(|root| real.starts_with(&root))
        .unwrap_or(false);
    let allowed = in_projects
        || asset_roots(app)
            .into_iter()
            .filter_map(|r| std::fs::canonicalize(r).ok())
            .any(|root| real.starts_with(&root));
    if !allowed {
        return Err(
            "来源必须在这几个目录里：图片 / 下载 / 桌面 / 文稿 / 音乐 / 影片 / 临时目录，或壁纸工程根"
                .into(),
        );
    }
    Ok(real)
}

/// 把宿主上的一个素材文件拷进工程（`project_import_asset`）。
///
/// 存在的意义：base64 走 JSON-RPC 写文件有 40 MB 上限（还要膨胀 4/3），而 AI 生成的
/// 4K 贴图、外部工具导出的 `.tex`/`.frag` 常常更大。这里直接走文件系统，既不占请求体
/// 也不占内存；安全边界见 [`check_asset_source`]。
pub fn import_asset(
    app: &AppHandle,
    name: &str,
    rel: &str,
    source: &str,
    overwrite: bool,
) -> Result<Value, String> {
    let dir = project_dir(app, name)?;
    if !dir.is_dir() {
        return Err(format!("工程不存在: {}", name.trim()));
    }
    reject_version_path(rel)?;
    let src = check_asset_source(app, source)?;
    let dest = safe_join(&dir, rel)?;
    if dest.is_dir() {
        return Err(format!("目标已存在同名目录: {rel}"));
    }
    if dest.is_file() && !overwrite {
        return Err(format!(
            "目标已存在: {rel}（要覆盖传 overwrite=true；或换个 path）"
        ));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::copy(&src, &dest).map_err(|e| format!("拷贝失败: {e}"))?;
    let bytes = dest.metadata().map(|m| m.len()).unwrap_or(0);
    // 与 project_write_file 同一套收尾：旧包失效 + 同名 .tex 冲突处理
    let stale_pkg = dir.join("scene.pkg");
    let invalidated = stale_pkg.is_file();
    if invalidated {
        let _ = std::fs::remove_file(&stale_pkg);
    }
    let (removed_tex, warning) = resolve_texture_siblings(&dir, &dest, rel);
    Ok(json!({
        "project": name.trim(),
        "path": rel_display(&dir, &dest),
        "bytes": bytes,
        "source": src.to_string_lossy(),
        "packInvalidated": invalidated,
        "removedStaleTex": removed_tex,
        "warning": warning,
    }))
}

/// 按本地库 item_id 反查工程名（工程目录名 → 导入 id 是确定性映射）
pub fn find_project_by_item(app: &AppHandle, item_id: &str) -> Result<Option<String>, String> {
    let list = list_projects(app)?;
    let hit = list
        .get("projects")
        .and_then(|p| p.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|p| {
                let name = p.get("project")?.as_str()?;
                (crate::library::project_item_id_for(name) == item_id).then(|| name.to_string())
            })
        });
    Ok(hit)
}

/// 把截图存成工程的 preview.png（安装后本地库卡片就有真实封面）
// 目前写封面那条路统一走 save_preview_as（要指定扩展名），这个 PNG 快捷入口暂无调用方
#[allow(dead_code)]
pub fn save_preview(app: &AppHandle, name: &str, png: &[u8]) -> Result<String, String> {
    save_preview_as(app, name, png, "png")
}

/// 同上，但可指定扩展名：渲染器自抓帧产出的是 **JPEG**（截图只用于看效果/当封面，
/// 走一次 HTTP 回传的 JPEG 比 4K PNG 小一个数量级）。本地库认 preview.png/jpg/...
/// 这一族扩展名，写对后缀才认得出封面。
pub fn save_preview_as(
    app: &AppHandle,
    name: &str,
    bytes: &[u8],
    ext: &str,
) -> Result<String, String> {
    let dir = project_dir(app, name)?;
    let ext = match ext {
        "jpg" | "jpeg" => "jpg",
        _ => "png",
    };
    // 换扩展名时清掉同名的另一份，别在工程里留两张封面
    if let Ok(old) = safe_join(
        &dir,
        if ext == "jpg" {
            "preview.png"
        } else {
            "preview.jpg"
        },
    ) {
        let _ = std::fs::remove_file(old);
    }
    let rel = format!("preview.{ext}");
    let dest = safe_join(&dir, &rel)?;
    std::fs::write(&dest, bytes).map_err(|e| format!("写入 {rel} 失败: {e}"))?;
    Ok(dest.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wpem-ws-test-{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 造一个最小可冻结工程（project.json 带 version/tags）
    fn versioned_project(tag: &str, version: u64) -> PathBuf {
        let dir = tmpdir(tag);
        std::fs::write(
            dir.join("project.json"),
            format!(r#"{{"type":"scene","title":"t","file":"scene.json","version":{version},"tags":["Everyone"]}}"#),
        )
        .unwrap();
        std::fs::write(dir.join("scene.json"), r#"{"objects":[]}"#).unwrap();
        std::fs::create_dir_all(dir.join("materials")).unwrap();
        std::fs::write(dir.join("materials/a.json"), r#"{"passes":[]}"#).unwrap();
        dir
    }

    /// `.version/` 历史副本不能被打进 scene.pkg（生成物/历史都不该上工坊）
    #[test]
    fn version_history_is_excluded_from_pack() {
        let dir = versioned_project("hist-excluded", 1);
        // 模拟旧版留下的历史副本目录
        let ver = dir.join(VERSION_DIR).join("1");
        std::fs::create_dir_all(&ver).unwrap();
        std::fs::write(ver.join("scene.json"), r#"{"objects":[]}"#).unwrap();
        std::fs::write(dir.join("materials/bg.png"), sample_png()).unwrap();

        let packed = pack_dir(&dir, "t").unwrap();
        let names = packed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(
            !names.iter().any(|n| n.starts_with(VERSION_DIR)),
            "包内不该有历史副本: {names:?}"
        );
    }

    /// 版本号与年龄分级是硬要求：缺了不给过（发布/回退/本地库筛选都靠它们）
    #[test]
    fn validate_requires_version_and_age_rating() {
        let dir = tmpdir("validate-version");
        std::fs::write(dir.join("index.html"), "<html></html>").unwrap();
        let write = |extra: &str| {
            std::fs::write(
                dir.join("project.json"),
                format!(r#"{{"type":"web","title":"t","file":"index.html"{extra}}}"#),
            )
            .unwrap();
            validate_dir(&dir, "t")
        };

        let r = write("");
        assert_eq!(r["ok"], json!(false));
        let errs = r["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(errs.contains("version"), "{errs}");
        assert!(errs.contains("年龄分级"), "{errs}");

        // 版本号必须是整数：字符串版本没法当目录名，也没法比较
        let r = write(r#","version":"1.0.0","tags":["Everyone"]"#);
        assert_eq!(r["ok"], json!(false));
        assert!(
            r["errors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e.as_str().unwrap().contains("version")),
            "{r}"
        );

        // 0 也不是合法版本号
        let r = write(r#","version":0,"tags":["Everyone"]"#);
        assert_eq!(r["ok"], json!(false));

        // 分级标签只能有一个，否则「这条壁纸几级」没有答案
        let r = write(r#","version":2,"tags":["Everyone","Mature"]"#);
        assert_eq!(r["ok"], json!(false));
        assert!(
            r["errors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e.as_str().unwrap().contains("多个年龄分级")),
            "{r}"
        );

        // 普通分类标签与分级共存 → 通过
        let r = write(r#","version":1,"tags":["Anime","Everyone"]"#);
        assert_eq!(r["ok"], json!(true), "{r}");
    }

    #[test]
    fn version_dir_is_write_protected() {
        for bad in [
            ".version",
            ".version/1/project.json",
            "./.version/1/x",
            ".version//1/x",
        ] {
            assert!(reject_version_path(bad).is_err(), "{bad} 必须拒绝");
        }
        for ok in ["project.json", "materials/a.json", "my.version/x"] {
            assert!(reject_version_path(ok).is_ok(), "{ok} 应当放行");
        }
    }

    /// 与渲染库同口径的 pkg 解析（逐字段对照 container.js），用于回读自检
    fn parse_pkg(buf: &[u8]) -> (String, Vec<(String, u32, u32)>, usize) {
        let u32at = |p: usize| u32::from_le_bytes(buf[p..p + 4].try_into().unwrap()) as usize;
        let magic_len = u32at(0);
        let magic = String::from_utf8_lossy(&buf[4..4 + magic_len]).to_string();
        assert!(magic.starts_with("PKGV"), "魔数: {magic}");
        let count = u32at(4 + magic_len);
        let mut p = 4 + magic_len + 4;
        let mut entries = Vec::new();
        for _ in 0..count {
            let name_len = u32at(p);
            p += 4;
            let name = String::from_utf8_lossy(&buf[p..p + name_len]).to_string();
            p += name_len;
            let offset = u32at(p);
            p += 4;
            let size = u32at(p);
            p += 4;
            entries.push((name, offset as u32, size as u32));
        }
        (magic, entries, p)
    }

    fn sample_png() -> Vec<u8> {
        // 1x1 PNG（从模板里取真图，保证是合法 PNG）
        include_bytes!("../templates/scene/materials/bg.png").to_vec()
    }

    #[test]
    fn safe_join_rejects_escape_and_absolute() {
        let root = tmpdir("safe");
        assert!(safe_join(&root, "../../etc/passwd").is_err());
        assert!(safe_join(&root, "/etc/passwd").is_err());
        assert!(safe_join(&root, "").is_err());
        assert!(safe_join(&root, "a/b.txt").is_ok());
        assert!(safe_join(&root, "./a/./b.txt").is_ok());
    }

    #[test]
    fn safe_join_rejects_symlink_component() {
        let root = tmpdir("symlink");
        let outside = tmpdir("symlink-out");
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let err = safe_join(&root, "link/x.txt").unwrap_err();
        assert!(err.contains("符号链接"), "{err}");
    }

    #[test]
    fn project_name_rules() {
        assert!(check_project_name("my-wallpaper").is_ok());
        assert!(check_project_name("极光壁纸").is_ok());
        assert!(check_project_name("").is_err());
        assert!(check_project_name(".hidden").is_err());
        assert!(check_project_name("a/b").is_err());
        assert!(check_project_name("a\\b").is_err());
        assert!(check_project_name(&"x".repeat(65)).is_err());
    }

    /// 模板必须开箱通过校验：漏掉 version / 年龄分级这类必填项会被立刻挡住
    #[test]
    fn templates_validate_out_of_the_box() {
        for kind in ["scene", "web"] {
            let dir = tmpdir(&format!("template-ok-{kind}"));
            for (rel, bytes) in template_files(kind).unwrap() {
                let dest = dir.join(rel);
                std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                std::fs::write(&dest, template_body(bytes, "模板自检")).unwrap();
            }
            let r = validate_dir(&dir, "t");
            assert_eq!(r["ok"], json!(true), "{kind} 模板不该有 error: {r}");
            let project = read_project_json(&dir).unwrap();
            assert_eq!(project_version(&project).unwrap(), 1, "{kind}");
            assert!(
                project_tags(&project).contains(&DEFAULT_RATING.to_string()),
                "{kind} 模板应默认大众级"
            );
        }
    }

    #[test]
    fn tex_from_png_matches_real_container_layout() {
        let png = sample_png();
        let tex = tex_from_image(&png, "png").unwrap();
        assert_eq!(&tex[0..9], b"TEXV0005\0");
        assert_eq!(&tex[9..18], b"TEXI0001\0");
        assert_eq!(u32::from_le_bytes(tex[18..22].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(tex[22..26].try_into().unwrap()), 2);
        let w = u32::from_le_bytes(tex[26..30].try_into().unwrap());
        let h = u32::from_le_bytes(tex[30..34].try_into().unwrap());
        assert_eq!((w, h), (1280, 720), "贴上真实尺寸");
        assert_eq!(&tex[46..55], b"TEXB0004\0");
        assert_eq!(u32::from_le_bytes(tex[55..59].try_into().unwrap()), 1);
        assert_eq!(i32::from_le_bytes(tex[59..63].try_into().unwrap()), 13);
        assert_eq!(u32::from_le_bytes(tex[63..67].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(tex[67..71].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(tex[71..75].try_into().unwrap()), w);
        assert_eq!(u32::from_le_bytes(tex[75..79].try_into().unwrap()), h);
        assert_eq!(u32::from_le_bytes(tex[79..83].try_into().unwrap()), 0);
        assert_eq!(i32::from_le_bytes(tex[83..87].try_into().unwrap()) as usize, png.len());
        assert_eq!(i32::from_le_bytes(tex[87..91].try_into().unwrap()) as usize, png.len());
        assert_eq!(&tex[91..], &png[..]);
    }

    #[test]
    fn build_pkg_is_self_consistent() {
        let entries = vec![
            ("materials/a.tex".to_string(), vec![1u8, 2, 3, 4]),
            ("scene.json".to_string(), b"{\"objects\":[]}".to_vec()),
        ];
        let pkg = build_pkg(&entries);
        let (magic, parsed, table_end) = parse_pkg(&pkg);
        assert_eq!(magic, PKG_MAGIC);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "materials/a.tex");
        assert_eq!(parsed[0].1, 0, "首个入口 offset 为 0");
        assert_eq!(parsed[1].1, 4, "offset 连续");
        let data_end = parsed
            .iter()
            .map(|(_, off, size)| table_end + *off as usize + *size as usize)
            .max()
            .unwrap();
        assert_eq!(data_end, pkg.len(), "数据段末尾贴住文件末尾");
    }

    #[test]
    fn parse_triple_accepts_we_wrappers() {
        assert_eq!(parse_triple(&json!("1 2 3")), Some([1.0, 2.0, 3.0]));
        assert_eq!(
            parse_triple(&json!({"value": "1.5 -2 0"})),
            Some([1.5, -2.0, 0.0])
        );
        assert!(parse_triple(&json!("1 2")).is_none());
        assert_eq!(parse_pair(&json!("1920 1080")), Some([1920.0, 1080.0]));
    }

    #[test]
    fn base64_write_and_read_roundtrip() {
        let data: Vec<u8> = (0u8..=255).collect();
        let enc = B64.encode(&data);
        assert_eq!(B64.decode(&enc).unwrap(), data);
        assert!(B64.decode("aGVsbG8").is_err(), "长度非法应报错");
    }

    #[test]
    fn validate_reports_missing_texture_and_bad_type() {
        let dir = tmpdir("validate");
        std::fs::create_dir_all(dir.join("materials")).unwrap();
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(
            dir.join("project.json"),
            r#"{"type":"scene","title":"t","general":{"properties":{"a":{"type":"slider","value":"x"}}}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("scene.json"),
            r#"{"general":{"orthogonalprojection":{"width":1920,"height":1080}},"camera":{},"objects":[
                {"id":1,"name":"L","image":"models/m.json","origin":"0 0 0","scale":"1 1 1","size":"10 10","angles":"0 0 0"}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("models/m.json"),
            r#"{"material":"materials/m.json"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("materials/m.json"),
            r#"{"passes":[{"shader":"genericimage2","textures":["missing"]}]}"#,
        )
        .unwrap();
        let report = validate_dir(&dir, "t");
        let errors = report["errors"].as_array().unwrap();
        assert!(errors.iter().any(|e| e.as_str().unwrap().contains("贴图缺失")));
        assert!(errors.iter().any(|e| e.as_str().unwrap().contains("value 与 type")));
    }

    /// 模板落盘必须按「是否合法 UTF-8」分流。曾经的实现一律走
    /// `String::from_utf8_lossy`，PNG 里的非法字节被换成 U+FFFD，贴图静默损坏，
    /// 一直到 scene_pack 才报「不是合法 PNG」。
    #[test]
    fn template_body_keeps_binary_assets_byte_identical() {
        let png = include_bytes!("../templates/scene/materials/bg.png");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "样本贴图应是 PNG 二进制");
        assert_eq!(
            template_body(png, "我的壁纸"),
            png.to_vec(),
            "二进制模板必须原样落盘"
        );

        let project = include_bytes!("../templates/scene/project.json");
        let out = String::from_utf8(template_body(project, "我的壁纸")).unwrap();
        assert!(out.contains("我的壁纸"), "文本模板要替换 {{TITLE}}");
        assert!(!out.contains("{{TITLE}}"), "占位符不能残留");

        // json/html/js/css 都过一遍：两个模板里所有文本文件替换后仍是合法 UTF-8
        for kind in template_kinds() {
            for (rel, bytes) in template_files(kind).unwrap() {
                let out = template_body(bytes, "标题");
                let is_png = rel.ends_with(".png");
                assert_eq!(
                    std::str::from_utf8(&out).is_err(),
                    is_png,
                    "{kind}/{rel} 的文本/二进制分类不对"
                );
            }
        }
    }

    /// 整条链路：模板落盘 → 校验 → 打包。这是端到端自检里真实走过的那段，
    /// 「模板贴图被当文本毁掉」的 bug 正是死在这里（校验只看文件是否存在，过不了打包）。
    #[test]
    fn scene_template_packs_end_to_end() {
        let dir = tmpdir("template-pack");
        for (rel, bytes) in template_files("scene").unwrap() {
            let dest = dir.join(rel);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(&dest, template_body(bytes, "自检")).unwrap();
        }

        // 刚建好的工程应当校验通过（只剩「还没打包」这类提示）
        let report = validate_dir(&dir, "t");
        assert_eq!(report["ok"], json!(true), "模板工程不该有 error: {report}");
        assert_eq!(report["type"], json!("scene"));

        // 打包（pack_scene 的主体；它只依赖目录，走的就是这里）
        let packed = pack_dir(&dir, "t").expect("模板工程应当能打包");
        assert!(packed["convertedTextures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "materials/bg.tex"));
        assert!(dir.join("scene.pkg").is_file());

        // 回读包：渲染要用的条目一个不少，且图片源已被 .tex 顶替
        let buf = std::fs::read(dir.join("scene.pkg")).unwrap();
        let (magic, parsed, table_end) = parse_pkg(&buf);
        assert_eq!(magic, "PKGV0022");
        let names: Vec<String> = parsed.iter().map(|(n, _, _)| n.clone()).collect();
        for want in [
            "scene.json",
            "models/background.json",
            "materials/background.json",
            "materials/bg.tex",
            "materials/presets/fireflies.json",
            "particles/presets/fireflies.json",
        ] {
            assert!(names.contains(&want.to_string()), "包内缺少 {want}: {names:?}");
        }
        assert!(
            !names.iter().any(|n| n.ends_with(".png")),
            "源图不该进包（应由 .tex 顶替）: {names:?}"
        );

        // 贴图容器里嵌的就是模板那张真图：尺寸与源文件一致
        let (_, off, size) = parsed
            .iter()
            .find(|(n, _, _)| n == "materials/bg.tex")
            .unwrap();
        let tex = &buf[table_end + *off as usize..table_end + (*off + *size) as usize];
        assert_eq!(&tex[..9], b"TEXV0005\0");
        let template_png = include_bytes!("../templates/scene/materials/bg.png");
        let (w, h) = png_dims(template_png).unwrap();
        let w_in_tex = u32::from_le_bytes(tex[26..30].try_into().unwrap());
        let h_in_tex = u32::from_le_bytes(tex[30..34].try_into().unwrap());
        assert_eq!((w_in_tex, h_in_tex), (w, h), "tex 里的尺寸应与源贴图一致");
    }

    /// 源图坏掉时，校验阶段就要报出来（而不是等 scene_pack 才失败）
    #[test]
    fn validate_reports_undecodable_texture_source() {
        let dir = tmpdir("validate-badtex");
        std::fs::create_dir_all(dir.join("materials")).unwrap();
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("project.json"), r#"{"type":"scene","title":"t"}"#).unwrap();
        std::fs::write(
            dir.join("scene.json"),
            r#"{"objects":[{"id":1,"name":"L","image":"models/m.json","origin":"0 0 0","scale":"1 1 1","size":"10 10","angles":"0 0 0"}]}"#,
        )
        .unwrap();
        std::fs::write(dir.join("models/m.json"), r#"{"material":"materials/m.json"}"#).unwrap();
        std::fs::write(
            dir.join("materials/m.json"),
            r#"{"passes":[{"shader":"genericimage2","textures":["tex1"]}]}"#,
        )
        .unwrap();
        // 被 from_utf8_lossy 毁过的样本（等价于旧实现落盘的结果）
        let mangled = String::from_utf8_lossy(include_bytes!("../templates/scene/materials/bg.png"))
            .replace("{{TITLE}}", "t")
            .into_bytes();
        std::fs::write(dir.join("materials/tex1.png"), &mangled).unwrap();

        let report = validate_dir(&dir, "t");
        assert_eq!(report["ok"], json!(false), "{report}");
        let errors: Vec<&str> = report["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect();
        assert!(
            errors.iter().any(|e| e.contains("无法解码")),
            "坏掉的源图应在校验阶段报错: {errors:?}"
        );
    }

    /// 同一张贴图被多个图层引用时，报告里不该出现重复条目
    #[test]
    fn validate_dedupes_repeated_reports() {
        let dir = tmpdir("validate-dedupe");
        std::fs::create_dir_all(dir.join("materials")).unwrap();
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("project.json"), r#"{"type":"scene","title":"t"}"#).unwrap();
        let layer = r#"{"id":1,"name":"L","image":"models/m.json","origin":"0 0 0","scale":"1 1 1","size":"10 10","angles":"0 0 0"}"#;
        std::fs::write(
            dir.join("scene.json"),
            format!(r#"{{"objects":[{layer},{layer}]}}"#).replace(r#""id":1"#, r#""id":2"#),
        )
        .unwrap();
        std::fs::write(dir.join("models/m.json"), r#"{"material":"materials/m.json"}"#).unwrap();
        std::fs::write(
            dir.join("materials/m.json"),
            r#"{"passes":[{"shader":"genericimage2","textures":["tex1"]}]}"#,
        )
        .unwrap();
        std::fs::write(dir.join("materials/tex1.png"), sample_png()).unwrap();

        let report = validate_dir(&dir, "t");
        let warnings = report["warnings"].as_array().unwrap();
        let mut uniq = std::collections::HashSet::new();
        for w in warnings {
            assert!(uniq.insert(w.as_str().unwrap().to_string()), "重复 warning: {w}");
        }
        // 两层同名同 id 之外完全一致 → 「没有 visible」这条必须只出现一次
        let hit = warnings
            .iter()
            .filter(|w| w.as_str().unwrap().contains("没有 visible"))
            .count();
        assert_eq!(hit, 1, "重复 warning 没去重: {warnings:?}");
    }

    #[test]
    fn validate_accepts_web_workspace() {
        let dir = tmpdir("validate-web");
        std::fs::write(
            dir.join("project.json"),
            r#"{"type":"web","title":"w","file":"index.html","version":1,"tags":["Everyone"],"general":{"properties":{"c":{"order":0,"type":"color","value":"1 0 0"},"m":{"order":1,"type":"combo","value":"a","options":[{"label":"A","value":"a"}]}}}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("index.html"), "<html></html>").unwrap();
        let report = validate_dir(&dir, "w");
        assert_eq!(report["ok"], json!(true), "{report}");
        assert_eq!(report["entry"], json!("index.html"));
    }

    #[test]
    fn pack_scene_writes_pkg_with_converted_texture() {
        let dir = tmpdir("pack");
        std::fs::create_dir_all(dir.join("materials")).unwrap();
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(
            dir.join("project.json"),
            r#"{"type":"scene","title":"p"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("scene.json"),
            r#"{"general":{"orthogonalprojection":{"width":1920,"height":1080}},"camera":{"eye":"0 0 0","center":"0 0 -1","up":"0 1 0"},"objects":[{"id":1,"name":"L","image":"models/m.json","origin":"1 2 3","scale":"1 1 1","size":"10 10","angles":"0 0 0","visible":true}]}"#,
        )
        .unwrap();
        std::fs::write(dir.join("models/m.json"), r#"{"material":"materials/m.json"}"#).unwrap();
        std::fs::write(
            dir.join("materials/m.json"),
            r#"{"passes":[{"shader":"genericimage2","textures":["tex1"]}]}"#,
        )
        .unwrap();
        std::fs::write(dir.join("materials/tex1.png"), sample_png()).unwrap();
        std::fs::write(dir.join("README.md"), "不该进包").unwrap();

        let mut raw = Vec::new();
        let mut total = 0;
        collect_pack_candidates(&dir, &dir, &mut raw, &mut total).unwrap();
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        for (rel, path) in raw {
            let bytes = std::fs::read(&path).unwrap();
            if rel.starts_with("materials/") && rel.ends_with(".png") {
                entries.push((replace_ext(&rel, "tex"), tex_from_image(&bytes, "png").unwrap()));
            } else {
                entries.push((rel, bytes));
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let pkg = build_pkg(&entries);
        let (_, parsed, table_end) = parse_pkg(&pkg);
        let names: Vec<String> = parsed.iter().map(|(n, _, _)| n.clone()).collect();
        assert!(names.contains(&"materials/tex1.tex".to_string()), "{names:?}");
        assert!(!names.iter().any(|n| n.ends_with(".png")), "{names:?}");
        assert!(!names.iter().any(|n| n.ends_with(".md")), "{names:?}");
        assert!(names.contains(&"scene.json".to_string()));
        let (_, off, size) = parsed
            .iter()
            .find(|(n, _, _)| n == "materials/tex1.tex")
            .unwrap();
        let slice = &pkg[table_end + *off as usize..table_end + (*off + *size) as usize];
        assert_eq!(&slice[..9], b"TEXV0005\0");
    }

    // ---------------------------------------------------------------- 场景字段级校验
    //
    // 这一组测试守的是「文档写了什么就校验什么」：坏场景必须在打包前被拦下来，
    // 而不是等装上桌面才发现效果静默消失（agent 拿不到渲染器的逐帧反馈）。

    /// 造一个可定制的场景工程（project.json 的属性表 + scene.json 由调用方给），
    /// 模型/材质/贴图按最简合法形态预置。
    fn scene_project(tag: &str, props: &str, scene: &str) -> PathBuf {
        let dir = tmpdir(tag);
        std::fs::write(
            dir.join("project.json"),
            format!(
                r#"{{"type":"scene","title":"t","file":"scene.json","version":1,"tags":["Everyone"],"general":{{"properties":{{{props}}}}}}}"#
            ),
        )
        .unwrap();
        std::fs::write(dir.join("scene.json"), scene).unwrap();
        std::fs::create_dir_all(dir.join("materials")).unwrap();
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("models/bg.json"), r#"{"material":"materials/bg.json"}"#).unwrap();
        std::fs::write(
            dir.join("materials/bg.json"),
            r#"{"passes":[{"shader":"genericimage2","textures":["bg"]}]}"#,
        )
        .unwrap();
        std::fs::write(dir.join("materials/bg.png"), sample_png()).unwrap();
        dir
    }

    fn joined(v: &Value, key: &str) -> String {
        v[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    /// 一层场景的最简骨架，`extra` 追加到第一个图层上
    fn scene_with_layer(extra: &str) -> String {
        format!(
            r#"{{"general":{{"orthogonalprojection":{{"width":1920,"height":1080}}}},"camera":{{}},
                "objects":[{{"id":1,"name":"L","image":"models/bg.json","origin":"960 540 0",
                "scale":"1 1 1","size":"1920 1080","angles":"0 0 0","visible":true{extra}}}]}}"#
        )
    }

    #[test]
    fn scene_accepts_bound_property_and_reports_none_unused() {
        let scene = scene_with_layer(r#","alpha":{"value":0.5,"user":"glow"}"#);
        let dir = scene_project(
            "scene-bind-ok",
            r#""glow":{"order":1,"type":"slider","value":0.5,"min":0,"max":1}"#,
            &scene,
        );
        let r = validate_dir(&dir, "t");
        assert_eq!(r["ok"], json!(true), "{r}");
        assert!(!joined(&r, "warnings").contains("没有任何图层绑定"), "{r}");
    }

    #[test]
    fn scene_rejects_unknown_property_binding() {
        // 绑了 project.json 里不存在的属性名：旧实现完全放过，画面静默不动
        let scene = scene_with_layer(r#","alpha":{"value":0.5,"user":"glow"}"#);
        let dir = scene_project("scene-bind-unknown", "", &scene);
        let r = validate_dir(&dir, "t");
        assert_eq!(r["ok"], json!(false), "{r}");
        assert!(joined(&r, "errors").contains("绑定了属性「glow」"), "{r}");
    }

    #[test]
    fn scene_warns_unused_and_skips_we_convention_props() {
        let scene = scene_with_layer("");
        let dir = scene_project(
            "scene-unused",
            r#""glow":{"order":1,"type":"slider","value":0.5},"schemecolor":{"order":0,"type":"color","value":"1 1 1"}"#,
            &scene,
        );
        let r = validate_dir(&dir, "t");
        assert_eq!(r["ok"], json!(true), "{r}");
        let w = joined(&r, "warnings");
        assert!(w.contains("没有任何图层绑定") && w.contains("glow"), "{w}");
        // schemecolor 是工坊浏览页配色，不该被算作「忘了绑」
        assert!(!w.contains("schemecolor"), "{w}");
    }

    #[test]
    fn scene_rejects_dangling_parent_and_cycle() {
        let scene = r#"{"general":{"orthogonalprojection":{"width":100,"height":100}},"camera":{},
            "objects":[
              {"id":1,"name":"A","image":"models/bg.json","origin":"50 50 0","scale":"1 1 1","size":"10 10","angles":"0 0 0","visible":true,"parent":999},
              {"id":2,"name":"B","origin":"50 50 0","scale":"1 1 1","size":"10 10","angles":"0 0 0","visible":true,"parent":3},
              {"id":3,"name":"C","origin":"50 50 0","scale":"1 1 1","size":"10 10","angles":"0 0 0","visible":true,"parent":2}]}"#;
        let dir = scene_project("scene-parent", "", scene);
        let r = validate_dir(&dir, "t");
        let errs = joined(&r, "errors");
        assert!(errs.contains("指向不存在的图层 id"), "{errs}");
        assert!(errs.contains("parent 链成环"), "{errs}");
    }

    #[test]
    fn scene_rejects_missing_effect_file() {
        let scene = scene_with_layer(
            r#","effects":[{"file":"effects/ghost.json","name":"ghost","visible":true}]"#,
        );
        let dir = scene_project("scene-effect", "", &scene);
        let r = validate_dir(&dir, "t");
        assert!(joined(&r, "errors").contains("找不到文件"), "{r}");
    }

    #[test]
    fn scene_rejects_bad_particle_material_and_warns_unknown_names() {
        let scene = scene_with_layer(r#","particle":"particles/presets/f.json""#);
        // image 与 particle 同时存在 → 只应有一条 warning，且 image 链仍然被校验
        let scene = scene.replace(r#""image":"models/bg.json","#, "");
        let dir = scene_project("scene-particle", "", &scene);
        std::fs::create_dir_all(dir.join("particles/presets")).unwrap();
        std::fs::write(
            dir.join("materials/p.json"),
            r#"{"passes":[{"shader":"genericimage2","textures":["bg"]}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("particles/presets/f.json"),
            r#"{"maxcount":5,"material":"materials/p.json",
                "emitter":[{"id":1,"name":"not_an_emitter"}],
                "initializer":[{"id":2,"name":"who_knows_random"}],
                "operator":[{"id":3,"name":"nope"}],
                "renderer":[{"id":4,"name":"not_a_renderer"}]}"#,
        )
        .unwrap();
        let r = validate_dir(&dir, "t");
        let errs = joined(&r, "errors");
        assert!(errs.contains("必须是 genericparticle"), "{errs}");
        let w = joined(&r, "warnings");
        for name in ["who_knows_random", "nope", "not_a_renderer"] {
            assert!(w.contains(name), "未知粒子组件没被提醒: {name}\n{w}");
        }
        // 发射器名字**不做白名单**：库只特判 boxrandom，其余一律按球壳发射，
        // 所以「名字不认识」不等于不生效，报了反而是误导
        assert!(!w.contains("not_an_emitter"), "{w}");
        assert!(w.contains("controlpoint"), "{w}");
    }

    #[test]
    fn scene_rejects_broken_animation() {
        let no_c0 = scene_with_layer(r#","brightness":{"value":1,"animation":{"options":{"fps":30,"length":300}}}"#);
        let dir = scene_project("scene-anim-noc0", "", &no_c0);
        let r = validate_dir(&dir, "t");
        assert!(joined(&r, "errors").contains("没有 c0"), "{r}");

        let bad_frames = scene_with_layer(
            r#","brightness":{"value":1,"animation":{"c0":[{"value":1}],"options":{"fps":0,"length":0}}}"#,
        );
        let dir = scene_project("scene-anim-frames", "", &bad_frames);
        let r = validate_dir(&dir, "t");
        let errs = joined(&r, "errors");
        assert!(errs.contains("缺少数字 frame"), "{errs}");
        assert!(errs.contains("fps=0"), "{errs}");
        assert!(errs.contains("length=0"), "{errs}");
    }

    #[test]
    fn scene_warns_layer_outside_design_and_zero_scale() {
        let scene = scene_with_layer("").replace(r#""origin":"960 540 0""#, r#""origin":"960 -400 0""#);
        let scene = scene.replace(r#""scale":"1 1 1""#, r#""scale":"0 1 1""#);
        let dir = scene_project("scene-geom", "", &scene);
        let r = validate_dir(&dir, "t");
        let w = joined(&r, "warnings");
        assert!(w.contains("落在设计分辨率"), "{w}");
        assert!(w.contains("有 0 分量"), "{w}");
    }

    #[test]
    fn texture_source_and_tex_collision_is_rejected() {
        let dir = scene_project("scene-tex-collide", "", &scene_with_layer(""));
        std::fs::write(dir.join("materials/bg.tex"), b"HANDWRITTEN").unwrap();
        let r = validate_dir(&dir, "t");
        assert_eq!(r["ok"], json!(false), "{r}");
        assert!(joined(&r, "errors").contains("同名冲突"), "{r}");
        // 打包器自己也兜一道底（pack_dir 也被直接调用）
        let err = pack_dir(&dir, "t").unwrap_err();
        assert!(err.contains("同名冲突"), "{err}");
    }

    #[test]
    fn writing_source_image_clears_stale_tex() {
        let dir = scene_project("scene-tex-clean", "", &scene_with_layer(""));
        std::fs::write(dir.join("materials/bg.tex"), b"HANDWRITTEN").unwrap();
        let dest = dir.join("materials/bg.png");
        let (removed, warn) = resolve_texture_siblings(&dir, &dest, "materials/bg.png");
        assert_eq!(removed, json!("materials/bg.tex"));
        assert!(!dir.join("materials/bg.tex").exists());
        assert!(warn.as_str().unwrap_or("").contains("已删除同名"), "{warn}");

        // 反向：写 .tex 时不动源图，但必须提醒它会盖住源图
        std::fs::write(dir.join("materials/bg.tex"), b"HANDWRITTEN").unwrap();
        let tex = dir.join("materials/bg.tex");
        let (removed, warn) = resolve_texture_siblings(&dir, &tex, "materials/bg.tex");
        assert!(removed.is_null());
        assert!(dir.join("materials/bg.png").exists(), "源图不该被删");
        assert!(warn.as_str().unwrap_or("").contains("会盖住"), "{warn}");
    }

    /// 工程模板自带的 glow 属性必须真的绑在图层上（否则用户拖滑块毫无反应）
    #[test]
    fn scene_template_binds_its_declared_property() {
        let files = template_files("scene").expect("scene 模板");
        let scene = files
            .iter()
            .find(|(p, _)| *p == "scene.json")
            .map(|(_, b)| std::str::from_utf8(b).unwrap().to_string())
            .unwrap();
        assert!(scene.contains(r#""user": "glow""#), "模板没绑定 glow");
    }

    // ---------------------------------------------------------------- 增量同步 / 体检

    /// 增量更新：只拷变化的、删源里没有的、不动没变的
    #[test]
    fn incremental_sync_copies_changes_and_prunes_deleted() {
        let src = tmpdir("sync-src");
        let dst = tmpdir("sync-dst");
        std::fs::create_dir_all(src.join("materials")).unwrap();
        std::fs::write(src.join("scene.json"), r#"{"objects":[]}"#).unwrap();
        std::fs::write(src.join("materials/bg.png"), sample_png()).unwrap();
        std::fs::write(src.join("materials/keep.png"), sample_png()).unwrap();

        // 首次：全部拷过去
        let s1 = sync_dir_filtered(&src, &dst).unwrap();
        assert_eq!(s1.copied, 3, "{s1:?}");
        assert!(dst.join("materials/bg.png").is_file());

        // 第二次：一个字节都不用拷
        let s2 = sync_dir_filtered(&src, &dst).unwrap();
        assert_eq!(s2.copied, 0, "{s2:?}");
        assert_eq!(s2.unchanged, 3, "{s2:?}");

        // 改一个 + 删一个：只拷改的、只删删掉的
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(src.join("scene.json"), r#"{"objects":[{"id":1}]}"#).unwrap();
        std::fs::remove_file(src.join("materials/keep.png")).unwrap();
        let s3 = sync_dir_filtered(&src, &dst).unwrap();
        assert_eq!(s3.copied, 1, "{s3:?}");
        assert_eq!(s3.removed, 1, "{s3:?}");
        assert_eq!(s3.unchanged, 1, "{s3:?}");
        assert!(!dst.join("materials/keep.png").exists(), "源里删掉的必须从库里清掉");
        assert!(dst.join("materials/bg.png").is_file(), "没变的素材不该被动过");

        // 版本历史与符号链接不进库（与 library 的拷贝口径一致）
        std::fs::create_dir_all(src.join(VERSION_DIR).join("1")).unwrap();
        std::fs::write(src.join(VERSION_DIR).join("1/scene.json"), "{}").unwrap();
        let s4 = sync_dir_filtered(&src, &dst).unwrap();
        assert_eq!(s4.copied, 0, "{s4:?}");
        assert!(!dst.join(VERSION_DIR).exists());

        // 库里多出来的东西（手动塞的）会被当成「源里没有」删掉
        std::fs::write(dst.join("stray.png"), b"x").unwrap();
        let s5 = sync_dir_filtered(&src, &dst).unwrap();
        assert_eq!(s5.removed, 1, "{s5:?}");
        assert!(!dst.join("stray.png").exists());
    }

    /// 体检：图层/贴图/未使用素材/绑定都要有内容
    #[test]
    fn inspect_reports_layers_textures_and_bindings() {
        let scene = scene_with_layer(r#","alpha":{"value":0.5,"user":"glow"}"#);
        let dir = scene_project(
            "inspect-ok",
            r#""glow":{"order":1,"type":"slider","value":0.5},"unused":{"order":2,"type":"slider","value":1}"#,
            &scene,
        );
        std::fs::write(dir.join("materials/orphan.png"), sample_png()).unwrap();
        let report = crate::scene_inspect::inspect_dir(&dir, "t");
        assert_eq!(report["layerCount"], json!(1), "{report}");
        assert_eq!(report["layers"][0]["name"], json!("L"));
        assert_eq!(report["layers"][0]["kind"], json!("image"));
        assert_eq!(report["layers"][0]["bindings"][0], json!("L.alpha → glow"));
        let tex = report["textures"].as_array().unwrap();
        assert!(tex.iter().any(|t| t["name"] == json!("bg")), "{report}");
        assert!(tex.iter().any(|t| t["width"] == json!(1280) && t["height"] == json!(720)), "贴图尺寸要报出来: {report}");
        let unused = report["unusedAssets"].as_array().unwrap();
        assert!(unused.iter().any(|u| u.as_str().unwrap().contains("orphan.png")), "{report}");
        assert_eq!(report["properties"]["unbound"], json!(["unused"]), "{report}");
        assert_eq!(report["ok"], json!(true), "{report}");
    }

    /// 素材导入白名单口径（目录判定需要 AppHandle，这里守扩展名与体积两条硬约束）
    #[test]
    fn asset_source_allowlist_rules() {
        assert!(!ASSET_EXT_ALLOW.contains(&"sh"));
        assert!(ASSET_EXT_ALLOW.contains(&"png"));
        assert!(ASSET_EXT_ALLOW.contains(&"frag"));
        assert!(MAX_ASSET_BYTES >= MAX_WRITE_BYTES as u64);
        assert_eq!(ASSET_EXT_ALLOW.len(), 13);
    }

    /// 支持清单（MCP 资源 + 校验器共用）里的名字必须真的出现在渲染库 bundle 里。
    /// 库侧改名/删实现而这里忘了同步时会失败；反向（库新增而这里没有）抓不到 ——
    /// 那种情况只会漏报 warning，不会误报。
    #[test]
    fn scene_support_names_exist_in_library_bundle() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let bundle = repo.join("node_modules/webwallgl/webwallgl.mjs");
        if !bundle.is_file() {
            eprintln!("跳过：{} 不存在（未装依赖）", bundle.display());
            return;
        }
        let src = std::fs::read_to_string(&bundle).unwrap();
        // initializer / operator 是 `switch (name)` 分派 → 精确断言 `case '<名字>':`
        // （第一版只查 contains，把文档里那个**并不存在**的 `sphererandom` 当成了实现名；
        //  它只出现在注释里，而库对发射器其实只特判 boxrandom）
        // bundle 由 esbuild 产出、字符串是双引号；两种引号都接受（换打包器不至于误报）
        let has = |needle_sq: &str, needle_dq: &str| {
            src.contains(needle_sq) || src.contains(needle_dq)
        };
        for name in crate::scene_inspect::switch_case_names() {
            assert!(
                has(&format!("case '{name}':"), &format!("case \"{name}\":")),
                "渲染库 bundle 里没有 `case \"{name}\":` —— 库侧改名/删实现后要同步 PARTICLE_* 常量"
            );
        }
        for name in crate::scene_inspect::literal_names() {
            assert!(
                has(&format!("'{name}'"), &format!("\"{name}\"")),
                "渲染库 bundle 里没有字面量 \"{name}\" —— 库侧改名/删实现后要同步 PARTICLE_* 常量"
            );
        }
        for kw in PARTICLE_TEXTURE_KEYWORDS {
            assert!(src.contains(kw), "bundle 里找不到粒子贴图关键词「{kw}」");
        }
        for m in BUILTIN_MODELS {
            let stem = m.rsplit('/').next().unwrap();
            assert!(src.contains(stem), "bundle 里找不到内置模型「{stem}」");
        }
    }

    /// 效果文件：WE 官方的 `material` 形态必须被接受（回归：校验器曾只认直写的 shader，
    /// 于是把官方 cursorripple 这类效果判成硬错误）
    #[test]
    fn effect_file_accepts_official_material_shape() {
        let dir = tmpdir("effect-material");
        std::fs::create_dir_all(dir.join("effects")).unwrap();
        std::fs::create_dir_all(dir.join("materials/effects")).unwrap();
        std::fs::create_dir_all(dir.join("shaders/effects")).unwrap();
        std::fs::write(dir.join("shaders/effects/myripple.frag"), "void main(){}").unwrap();
        // 成对提供 .vert —— 只给 .frag 是「静默跳过」的经典坑，由下面第 ⑤ 段单独覆盖
        std::fs::write(dir.join("shaders/effects/myripple.vert"), "void main(){}").unwrap();
        std::fs::write(
            dir.join("materials/effects/myripple.json"),
            r#"{"passes":[{"shader":"effects/myripple","blending":"normal"}]}"#,
        )
        .unwrap();
        // ① 官方形态：passes[].material → 材质里的 shader 在工程里存在
        std::fs::write(
            dir.join("effects/ok.json"),
            r#"{"passes":[{"material":"materials/effects/myripple.json","target":null,"bind":[]},
                          {"command":"swap","source":"_rt_A","target":"_rt_B"}]}"#,
        )
        .unwrap();
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        check_effect_file(&dir, "effects/ok.json", "L", "ok", &mut errors, &mut warnings);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(warnings.is_empty(), "{warnings:?}");

        // ② 引用的材质不存在 → 硬错误
        std::fs::write(
            dir.join("effects/missing.json"),
            r#"{"passes":[{"material":"materials/effects/nope.json"}]}"#,
        )
        .unwrap();
        errors.clear();
        check_effect_file(&dir, "effects/missing.json", "L", "m", &mut errors, &mut warnings);
        assert!(errors.iter().any(|e| e.contains("引用的材质不存在")), "{errors:?}");

        // ③ 效果文件里直写 shader → 必须警告「渲染库只认 material」（会被静默丢弃）
        std::fs::write(
            dir.join("effects/plain.json"),
            r#"{"passes":[{"shader":"effects/myripple"}]}"#,
        )
        .unwrap();
        errors.clear();
        warnings.clear();
        check_effect_file(&dir, "effects/plain.json", "L", "p", &mut errors, &mut warnings);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(
            warnings.iter().any(|w| w.contains("只认 material")),
            "直写 shader 必须被点破: {warnings:?}"
        );

        // ④ 既没 material / shader 也不是 command → 硬错误
        std::fs::write(dir.join("effects/bad.json"), r#"{"passes":[{"target":"_rt_X"}]}"#).unwrap();
        errors.clear();
        check_effect_file(&dir, "effects/bad.json", "L", "b", &mut errors, &mut warnings);
        assert!(errors.iter().any(|e| e.contains("既没有 material")), "{errors:?}");

        // ⑤ 只有 .frag、缺 .vert → 必须警告（渲染库会静默跳过这一趟）
        std::fs::remove_file(dir.join("shaders/effects/myripple.vert")).unwrap();
        errors.clear();
        warnings.clear();
        check_effect_file(&dir, "effects/ok.json", "L", "ok", &mut errors, &mut warnings);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(
            warnings.iter().any(|w| w.contains("缺 shaders/effects/myripple.vert")),
            "缺 .vert 必须主动警告: {warnings:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// angles 带关键帧动画**不再告警** —— 渲染库 38e877c 已修（#9），常驻警报会变成噪音。
    /// 这条测试是"防止误报回归"的闸门：如果哪天有人再加回 angles 告警，这里会红。
    #[test]
    fn angles_animation_is_not_warned_anymore() {
        let dir = tmpdir("angles-no-warn");
        std::fs::create_dir_all(dir.join("materials")).unwrap();
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("project.json"),
            r#"{"type":"scene","title":"t","file":"scene.json","version":1,"tags":["Everyone"],"general":{"properties":{}}}"#).unwrap();
        std::fs::write(dir.join("materials/bg.json"), r#"{"passes":[{"shader":"genericimage2","textures":[]}]}"#).unwrap();
        std::fs::write(dir.join("models/bg.json"), r#"{"autosize":true,"material":"materials/bg.json"}"#).unwrap();
        std::fs::write(dir.join("scene.json"), r#"{"general":{"orthogonalprojection":{"width":1920,"height":1080}},"objects":[
            {"id":1,"name":"A","image":"models/bg.json","origin":"960 540 0","size":"1920 1080",
             "angles":{"value":"0 0 0","animation":{"c0":[{"frame":0,"value":"0 0 -0.05"},{"frame":150,"value":"0 0 0.05"}],
             "options":{"fps":30,"length":300,"mode":"loop"}}}}]}"#).unwrap();
        let v = validate_dir(&dir, "t");
        assert!(v["ok"].as_bool().unwrap_or(false), "{v}");
        let hits: Vec<String> = v["warnings"].as_array().unwrap().iter()
            .map(|w| w.as_str().unwrap_or("").to_string())
            .filter(|w| w.contains("angles") || w.contains("webwallgl#9"))
            .collect();
        assert!(hits.is_empty(), "angles 动画不该再告警（库已修 38e877c）: {hits:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scene_support_json_is_serializable_and_complete() {
        let v = scene_support_json();
        assert!(v["shaders"]["builtin"].as_array().unwrap().len() >= 4);
        assert!(v["particles"]["initializer"].as_array().unwrap().len() >= 8);
        assert_eq!(v["particles"]["material_shader"], json!("genericparticle"));
        assert_eq!(v["limits"]["writeFileBytes"], json!(MAX_WRITE_BYTES as u64));
    }
}
