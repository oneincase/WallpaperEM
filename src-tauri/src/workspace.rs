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
//!   + 每个 mip：`mipCount` + `mw mh` + `compression` + `uncompressedSize` + `compressedSize` + 数据。
//!   hasMipExtension=0 → 解析方按 V3 布局读（见 vendor 的 parseTex）。
//!   freeImageFormat=13(PNG)/2(JPEG) 时数据段直接是整张图片文件，无需解码重编码。
//!   **不要用 WebP**：`FIF.WEBP === 35 === FIF.MP4`，解析方会把它当视频纹理吞掉。

use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::{json, Value};
use tauri::AppHandle;

/// 工程根目录名（文档目录下一级）
const PROJECTS_DIRNAME: &str = "WallpaperEM";
const PROJECTS_SUBDIR: &str = "Projects";
/// 版本历史目录：`.version/<版本号>/` 存该版源文件的完整副本
pub(crate) const VERSION_DIR: &str = ".version";
/// 每个版本目录里的元数据（隐藏文件，树遍历一律跳过，不会混进工程/包/本地库）
const SNAPSHOT_META: &str = ".snapshot.json";
/// 年龄分级标签（与 `src/lib/tags.ts` 的「年龄分级」组同口径）。
/// 值必须与 Steam 工坊标签**逐字符一致**：它们既走筛选也随发布带给 Steam。
pub(crate) const RATING_TAGS: [&str; 3] = ["Everyone", "Questionable", "Mature"];
/// 默认分级：大众级
pub(crate) const DEFAULT_RATING: &str = "Everyone";
/// 单个工程目录名长度上限（含中文按字符数算）
const MAX_PROJECT_NAME_CHARS: usize = 64;
/// 工程内单文件写入上限（base64 解码后）
const MAX_WRITE_BYTES: usize = 512 << 20;
/// 打包时工程总大小上限（防止把整个图库塞进一个 pkg）
const MAX_PACK_BYTES: u64 = 2 << 30;
const PKG_MAGIC: &str = "PKGV0022";

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

/// `.version/` 只由 project_snapshot / project_rollback 维护：允许 agent 直接往里写，
/// 等于绕开「历史只增不改」，将来回退会拿到一份被改过的「第 N 版」。
///
/// 只看**第一个有效路径段**（跳过空段与 `.`），否则 `./.version/1/x` 这种写法能绕过检查。
fn reject_version_path(rel: &str) -> Result<(), String> {
    let norm = rel.trim().replace('\\', "/");
    let first = norm.split('/').find(|s| !s.is_empty() && *s != ".");
    if first == Some(VERSION_DIR) {
        return Err(format!(
            "{VERSION_DIR}/ 是版本历史目录，不能直接写入；用 project_snapshot / project_rollback 管理版本"
        ));
    }
    Ok(())
}

/// 把工程内的相对路径解析成绝对路径，并保证逃不出工程根。
///
/// 三道闸：拒绝绝对路径/盘符、拒绝 `..`、逐级拒绝符号链接（否则
/// `ln -s /etc passwd` 之后写 `passwd/hosts` 就能改到工程外）。
fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
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
fn rel_display(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| p.to_string_lossy().to_string())
}

// ---------------------------------------------------------------- 工程读写

/// 列出工作区里的全部工程
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
    Ok(json!({
        "project": name.trim(),
        "path": rel_display(&dir, &dest),
        "bytes": bytes.len(),
        "packInvalidated": invalidated,
    }))
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
    let is_text = matches!(
        std::str::from_utf8(&bytes),
        Ok(_)
    );
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

// ---------------------------------------------------------------- 版本历史

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 读工程根下的 project.json（解析失败当作没有）
fn read_project_json(dir: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(dir.join("project.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// 版本号：`project.json` 里的 `version`，必须是 ≥ 1 的整数。
///
/// 它同时是 `.version/<n>/` 的目录名，所以不接受字符串（`"1.0.0"` 没法当目录名排序，
/// 版本比较也会变成字符串比较）。发布与回退都靠它，缺了就没法定位历史版本。
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

/// 容错版：老工程没有 version 时按第 1 版处理（首次冻结时顺手补上）
fn version_or_first(project: &Value) -> u64 {
    project_version(project).unwrap_or(1)
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

/// 工程里的「源文件」：相对路径 → 绝对路径。跳过隐藏项（含 `.version/` 与快照元数据）
/// 与打包产物 `scene.pkg`——生成物能重建，进历史只会让快照体积翻倍。
fn collect_source_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
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
            collect_source_files(root, &path, out)?;
        } else if ft.is_file() {
            out.push((rel_display(root, &path), path));
        }
    }
    Ok(())
}

/// `project.json` 去掉 version 后的规范形式：比较「有没有改动」时不该被版本号本身带偏
/// （快照会把工作副本的 version 推进一版，那不是内容改动）。
fn normalized_project_json(bytes: &[u8]) -> Option<String> {
    let mut v: Value = serde_json::from_slice(bytes).ok()?;
    if let Some(obj) = v.as_object_mut() {
        obj.remove("version");
    }
    serde_json::to_string(&v).ok()
}

/// 两棵工程树的内容是否一致（符号链接/生成物已由 collect_source_files 排除）
fn same_tree(a: &Path, b: &Path, ignore_version: bool) -> bool {
    let mut fa = Vec::new();
    let mut fb = Vec::new();
    if collect_source_files(a, a, &mut fa).is_err() || collect_source_files(b, b, &mut fb).is_err() {
        return false;
    }
    fa.sort_by(|x, y| x.0.cmp(&y.0));
    fb.sort_by(|x, y| x.0.cmp(&y.0));
    if fa.len() != fb.len() {
        return false;
    }
    for ((ra, pa), (rb, pb)) in fa.iter().zip(fb.iter()) {
        if ra != rb {
            return false;
        }
        let (Ok(ba), Ok(bb)) = (std::fs::read(pa), std::fs::read(pb)) else {
            return false;
        };
        if ba == bb {
            continue;
        }
        if ignore_version && ra == "project.json" {
            if normalized_project_json(&ba) == normalized_project_json(&bb) {
                continue;
            }
        }
        return false;
    }
    true
}

/// `.version/` 下已冻结的最大版本号（没有历史时为 0）
fn max_frozen_version(vroot: &Path) -> Result<u64, String> {
    let mut max = 0u64;
    let Ok(entries) = std::fs::read_dir(vroot) else {
        return Ok(0);
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if let Ok(n) = entry.file_name().to_string_lossy().parse::<u64>() {
            max = max.max(n);
        }
    }
    Ok(max)
}

/// 写回 project.json 的 version（保留其余字段；文件会被重排成 2 空格缩进）
fn set_project_version(dir: &Path, version: u64) -> Result<(), String> {
    let mut project = read_project_json(dir).ok_or("project.json 不存在或不是合法 JSON")?;
    let Some(obj) = project.as_object_mut() else {
        return Err("project.json 顶层必须是对象".into());
    };
    obj.insert("version".into(), json!(version));
    let text = serde_json::to_string_pretty(&project).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("project.json"), format!("{text}\n"))
        .map_err(|e| format!("写入 project.json 失败: {e}"))
}

/// 冻结当前工作副本到 `.version/<版本号>/`，并让工作副本进入下一版。
///
/// 编号规则：目标号默认取 `project.json` 的 version；那一版已经存在时**从不覆盖**，
/// 而是顺延到「已冻结的最大号 + 1」——历史只增不改，回退过的工程才能继续往前走
/// （回退到第 1 版后接着改，不会把第 1 版的历史冲掉）。
pub fn snapshot_project(app: &AppHandle, name: &str, note: Option<&str>) -> Result<Value, String> {
    let dir = project_dir(app, name)?;
    if !dir.is_dir() {
        return Err(format!("工程不存在: {}", name.trim()));
    }
    snapshot_dir(&dir, name.trim(), note)
}

/// 冻结主体（不碰 AppHandle，便于直接对目录单测）
fn snapshot_dir(dir: &Path, name: &str, note: Option<&str>) -> Result<Value, String> {
    let project = read_project_json(&dir).ok_or("project.json 不存在或不是合法 JSON")?;
    let current = version_or_first(&project);

    let mut files = Vec::new();
    collect_source_files(&dir, &dir, &mut files)?;
    if files.is_empty() {
        return Err("工程里没有任何源文件可冻结".into());
    }
    let total: u64 = files
        .iter()
        .filter_map(|(_, p)| p.metadata().ok())
        .map(|m| m.len())
        .sum();
    if total > MAX_PACK_BYTES {
        return Err(format!(
            "工程过大（{total} 字节 > 上限 {MAX_PACK_BYTES}），冻结历史前请先精简素材"
        ));
    }

    let vroot = dir.join(VERSION_DIR);
    let mut n = current;
    let mut adjusted = Value::Null;
    if vroot.join(n.to_string()).is_dir() {
        if same_tree(&dir, &vroot.join(n.to_string()), false) {
            return Ok(json!({
                "project": name.trim(),
                "frozen": false,
                "version": n,
                "current": current,
                "reason": format!("工作副本与已冻结的第 {n} 版完全一致，无需重复冻结"),
            }));
        }
        n = max_frozen_version(&vroot)? + 1;
        adjusted = json!(format!(
            "第 {current} 版已冻结且内容不同，本次顺延为第 {n} 版（历史不覆盖）"
        ));
    }

    let dest = vroot.join(n.to_string());
    // 拷贝到一半失败（磁盘满等）必须把半成品清掉：留着会被当成一个「有效版本」，
    // 之后 max_frozen_version 会把号段算错，甚至回退到一份残缺的副本。
    let write_snapshot = || -> Result<(), String> {
        std::fs::create_dir_all(&dest).map_err(|e| format!("创建版本目录失败: {e}"))?;
        for (rel, path) in &files {
            let target = safe_join(&dest, rel)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let bytes = std::fs::read(path).map_err(|e| format!("读取 {rel} 失败: {e}"))?;
            // 顺延过的版本号要写进快照自己的 project.json：目录名与声明的版本必须一致，
            // 否则将来回退到它，工作副本会带着一个对不上号（或更小）的版本号回去。
            let bytes = if rel == "project.json" && n != current {
                let mut v: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("version".into(), json!(n));
                }
                serde_json::to_vec_pretty(&v).map_err(|e| e.to_string())?
            } else {
                bytes
            };
            std::fs::write(&target, &bytes).map_err(|e| format!("写入 {rel} 失败: {e}"))?;
        }
        Ok(())
    };
    if let Err(e) = write_snapshot() {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(format!("冻结第 {n} 版失败（已清理半成品）: {e}"));
    }
    let meta = json!({
        "version": n,
        "note": note.map(|s| s.trim()).filter(|s| !s.is_empty()),
        "at": now_secs(),
        "files": files.len(),
        "bytes": total,
    });
    std::fs::write(
        dest.join(SNAPSHOT_META),
        serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("写入快照元数据失败: {e}"))?;

    // 工作副本进入下一版。project.json 内嵌在 scene.pkg 里，版本号一变包就过期。
    set_project_version(&dir, n + 1)?;
    let pkg = dir.join("scene.pkg");
    let invalidated = pkg.is_file();
    if invalidated {
        let _ = std::fs::remove_file(&pkg);
    }

    Ok(json!({
        "project": name.trim(),
        "frozen": true,
        "version": n,
        "path": dest.to_string_lossy(),
        "files": files.len(),
        "bytes": total,
        "current": n + 1,
        "packInvalidated": invalidated,
        "adjusted": adjusted,
        "hint": if invalidated {
            "工作副本已进入下一版；scene.pkg 已失效，需要时重新 scene_pack"
        } else {
            "工作副本已进入下一版"
        },
    }))
}

/// 列出历史版本（按版本号升序），并给出工作副本当前的版本号
pub fn list_project_versions(app: &AppHandle, name: &str) -> Result<Value, String> {
    let dir = project_dir(app, name)?;
    if !dir.is_dir() {
        return Err(format!("工程不存在: {}", name.trim()));
    }
    versions_dir(&dir, name.trim())
}

/// 列版本主体（不碰 AppHandle，便于单测）
fn versions_dir(dir: &Path, name: &str) -> Result<Value, String> {
    let current = read_project_json(&dir).map(|p| version_or_first(&p)).unwrap_or(1);
    let vroot = dir.join(VERSION_DIR);
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&vroot) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Ok(n) = entry.file_name().to_string_lossy().parse::<u64>() else {
                continue; // 非数字目录不是版本
            };
            let meta: Value = std::fs::read_to_string(path.join(SNAPSHOT_META))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null);
            let mut files = Vec::new();
            let _ = collect_source_files(&path, &path, &mut files);
            let bytes: u64 = files
                .iter()
                .filter_map(|(_, p)| p.metadata().ok())
                .map(|m| m.len())
                .sum();
            out.push(json!({
                "version": n,
                "note": meta.get("note").cloned().unwrap_or(Value::Null),
                "at": meta.get("at").cloned().unwrap_or(json!(0)),
                "files": files.len(),
                "bytes": bytes,
                "path": path.to_string_lossy(),
            }));
        }
    }
    out.sort_by_key(|v| v["version"].as_u64().unwrap_or(0));
    Ok(json!({
        "project": name.trim(),
        "current": current,
        "root": vroot.to_string_lossy(),
        "count": out.len(),
        "versions": out,
    }))
}

/// 回退到某个历史版本：把该版文件还原到工程根，并删掉工程根里该版没有的源文件
/// （等于 checkout 那一版）。
///
/// 默认拒绝「有未冻结改动」的回退：改动没冻进历史就回退，等于把它悄悄丢掉。
pub fn rollback_project(
    app: &AppHandle,
    name: &str,
    version: u64,
    force: bool,
) -> Result<Value, String> {
    let dir = project_dir(app, name)?;
    if !dir.is_dir() {
        return Err(format!("工程不存在: {}", name.trim()));
    }
    rollback_dir(&dir, name.trim(), version, force)
}

/// 回退主体（不碰 AppHandle，便于直接对目录单测）
fn rollback_dir(dir: &Path, name: &str, version: u64, force: bool) -> Result<Value, String> {
    let vroot = dir.join(VERSION_DIR);
    let src = vroot.join(version.to_string());
    if !src.is_dir() {
        return Err(format!(
            "第 {version} 版不存在（用 project_versions 看有哪些版本）"
        ));
    }

    let latest = max_frozen_version(&vroot)?;
    let dirty = latest == 0 || !same_tree(&dir, &vroot.join(latest.to_string()), true);
    if dirty && !force {
        return Err(format!(
            "工作副本与最新冻结的第 {latest} 版不一致（有未冻结的改动）。\
             确认丢弃请传 force=true，或先 project_snapshot 把当前状态存一版"
        ));
    }

    let mut want = Vec::new();
    collect_source_files(&src, &src, &mut want)?;
    let mut restored = 0usize;
    for (rel, path) in &want {
        let target = safe_join(&dir, rel)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::copy(path, &target).map_err(|e| format!("还原 {rel} 失败: {e}"))?;
        restored += 1;
    }

    // 快照里没有的源文件要删掉，否则工程根是「新版 ∪ 旧版」的混合体
    let keep: std::collections::HashSet<&str> = want.iter().map(|(rel, _)| rel.as_str()).collect();
    let mut now = Vec::new();
    collect_source_files(&dir, &dir, &mut now)?;
    let mut removed = Vec::new();
    for (rel, path) in &now {
        if !keep.contains(rel.as_str()) {
            std::fs::remove_file(path).map_err(|e| format!("删除 {rel} 失败: {e}"))?;
            removed.push(rel.clone());
        }
    }
    prune_empty_dirs(&dir);

    // 源回退了，包必然过期
    let pkg = dir.join("scene.pkg");
    let invalidated = pkg.is_file();
    if invalidated {
        let _ = std::fs::remove_file(&pkg);
    }

    Ok(json!({
        "project": name.trim(),
        "version": version,
        "current": version,
        "restored": restored,
        "removed": removed,
        "packInvalidated": invalidated,
        "hint": "源文件已回到该版本；scene.pkg 需要重新 scene_pack",
    }))
}

/// 清掉工程里的空目录（回退后可能留下空壳）。不碰 `.version/`。
fn prune_empty_dirs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || !path.is_dir() {
            continue;
        }
        prune_empty_dirs(&path);
        if std::fs::read_dir(&path).map(|d| d.count() == 0).unwrap_or(false) {
            let _ = std::fs::remove_dir(&path);
        }
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

fn validate_dir(dir: &Path, name: &str) -> Value {
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
                    Some(scene) => check_scene(dir, &scene, &mut errors, &mut warnings),
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

/// scene.json 校验：只校验 v1 明确支持的子集，未知字段一律放过（WE 原生格式）
fn check_scene(dir: &Path, scene: &Value, errors: &mut Vec<String>, warnings: &mut Vec<String>) {
    let general = scene.get("general");
    match general
        .and_then(|g| g.get("orthogonalprojection"))
        .map(|o| (o.get("width"), o.get("height")))
    {
        Some((Some(w), Some(h))) if w.as_f64().unwrap_or(0.0) > 0.0 && h.as_f64().unwrap_or(0.0) > 0.0 => {}
        _ => warnings.push(
            "scene.json 的 general.orthogonalprojection 缺 width/height（渲染分辨率会退化成窗口尺寸）".into(),
        ),
    }
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
    let mut ids = std::collections::HashSet::new();
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
        if let Some(rel) = obj.get("image").and_then(|v| v.as_str()) {
            check_model_chain(dir, rel, &label, errors, warnings);
        }
        if let Some(rel) = obj.get("particle").and_then(|v| v.as_str()) {
            if !safe_join(dir, rel).map(|p| p.is_file()).unwrap_or(false) {
                errors.push(format!("对象 {label} 引用的粒子预设不存在: {rel}"));
            }
        }
        if obj.get("visible").is_none() {
            warnings.push(format!("对象 {label} 没有 visible（默认按可见处理）"));
        }
    }
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
                let has_src = ["png", "jpg", "jpeg"]
                    .iter()
                    .any(|ext| dir.join(format!("materials/{name}.{ext}")).is_file());
                if !has_tex && !has_src {
                    errors.push(format!(
                        "材质 {mat_rel} 引用的贴图缺失：需要 materials/{name}.tex 或 materials/{name}.png"
                    ));
                } else if !has_tex {
                    // 源图能不能解码要在这里就报错：等 scene_pack 才失败的话，
                    // agent 拿到的只是「转换失败」，看不出是源图本身坏了。
                    match image_source_dims(dir, name) {
                        Ok(_) => warnings.push(format!(
                            "贴图 materials/{name}.png 还没转成 .tex（scene_pack 会自动转换）"
                        )),
                        Err(e) => errors.push(format!(
                            "贴图 materials/{name} 无法解码（{e}）；scene_pack 转不出 .tex"
                        )),
                    }
                }
            }
        }
    }
}

/// 读 materials/<name>.(png|jpg|jpeg) 的尺寸，顺带验证它确实是张能解出来的图。
/// 返回 (相对路径, 宽, 高)。
fn image_source_dims(dir: &Path, name: &str) -> Result<(String, u32, u32), String> {
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

/// WE 内置反照率直通 shader（渲染库用 copy pass 画，不需要 pkg 内嵌源码）
fn is_builtin_shader(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(n.as_str(), "generic" | "sprite" | "flat" | "genericparticle")
        || n.starts_with("genericimage")
}

fn parse_triple(v: &Value) -> Option<[f64; 3]> {
    parse_floats(v, 3).map(|mut f| {
        let mut out = [0.0; 3];
        out[..3].copy_from_slice(&f.drain(..3).collect::<Vec<_>>());
        out
    })
}

fn parse_pair(v: &Value) -> Option<[f64; 2]> {
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
pub fn pack_scene(app: &AppHandle, name: &str) -> Result<Value, String> {
    let dir = project_dir(app, name)?;
    if !dir.is_dir() {
        return Err(format!("工程不存在: {}", name.trim()));
    }
    pack_dir(&dir, name.trim())
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
            if entries.iter().any(|(n, _)| *n == tex_rel) {
                continue; // 同名 .tex 已存在 → 手写的那份优先
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
fn tex_from_image(bytes: &[u8], ext: &str) -> Result<Vec<u8>, String> {
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

fn png_dims(bytes: &[u8]) -> Result<(u32, u32), String> {
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
fn jpeg_dims(bytes: &[u8]) -> Result<(u32, u32), String> {
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

/// 覆盖安装：先用同一 item_id 重新导入，失败时把旧版本原样放回去。
/// item_id 由工程目录名清洗得到（非 ASCII 名字回退到稳定哈希），目录名不变时 id 不变，
/// 用户改过的属性覆盖（settings 表按 item_id 存）也就不丢。
pub fn update_project(
    app: &AppHandle,
    name: &str,
    item_id: Option<&str>,
) -> Result<Value, String> {
    let target = match item_id.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(id) => id.to_string(),
        None => crate::library::project_item_id_for(name.trim()),
    };
    let lib_dir = crate::library::item_dir(app, &target)?;
    // 备份就地放在库根下但以 `.` 开头：库扫描会跳过隐藏目录，同卷 rename 也不会
    // 因跨文件系统而失败。
    let backup = lib_dir.with_file_name(format!(".{}.bak-{}", target, std::process::id()));
    let moved = stage_aside(&lib_dir, &backup)?;

    match install_project(app, name) {
        Ok((new_id, title, wtype)) => {
            if moved {
                let _ = std::fs::remove_dir_all(&backup);
            }
            Ok(json!({
                "project": name.trim(),
                "itemId": new_id,
                "previousItemId": target,
                "idChanged": new_id != target,
                "title": title,
                "type": wtype,
            }))
        }
        Err(e) => {
            restore_aside(&lib_dir, &backup, moved);
            Err(e)
        }
    }
}

/// 更新前让位：把旧安装目录改名到隐藏备份路径，返回是否真的移过。
///
/// 三种做法里只有改名是对的：
/// - 直接删旧目录 → 新版本导入失败就「更新失败 = 壁纸没了」；
/// - 先装再删 → `import_into_library` 用 `unique_dest_dir` 避让同名目录，会退到 `-1`
///   后缀，item_id 一变，用户改过的属性覆盖整条丢失；
/// - 改名 → 原 id 腾空（新旧不混），失败还能放回去（见 `restore_aside`）。
fn stage_aside(dir: &Path, backup: &Path) -> Result<bool, String> {
    if !dir.is_dir() {
        return Ok(false);
    }
    let _ = std::fs::remove_dir_all(backup); // 上一次崩在半路留下的残壳
    std::fs::rename(dir, backup).map_err(|e| format!("暂存旧安装目录失败: {e}"))?;
    Ok(true)
}

/// 回滚：丢掉写了一半的新目录，把备份放回原 id。尽力而为，失败也不该盖住原始错误。
fn restore_aside(dir: &Path, backup: &Path, moved: bool) {
    if !moved {
        return;
    }
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::rename(backup, dir);
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
pub fn save_preview(app: &AppHandle, name: &str, png: &[u8]) -> Result<String, String> {
    let dir = project_dir(app, name)?;
    let dest = safe_join(&dir, "preview.png")?;
    std::fs::write(&dest, png).map_err(|e| format!("写入 preview.png 失败: {e}"))?;
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

    fn read_version(dir: &Path) -> u64 {
        read_project_json(dir).map(|p| version_or_first(&p)).unwrap()
    }

    #[test]
    fn snapshot_freezes_files_and_advances_working_version() {
        let dir = versioned_project("snap-basic", 1);
        // 打包产物不该进历史（能重建，进历史等于体积翻倍）
        std::fs::write(dir.join("scene.pkg"), b"PKGV0022-stale").unwrap();

        let r = snapshot_dir(&dir, "t", Some("初版")).unwrap();
        assert_eq!(r["frozen"], json!(true));
        assert_eq!(r["version"], json!(1));
        assert_eq!(r["current"], json!(2), "工作副本应进入下一版");
        assert_eq!(read_version(&dir), 2);

        let snap = dir.join(VERSION_DIR).join("1");
        assert!(snap.join("project.json").is_file());
        assert!(snap.join("scene.json").is_file());
        assert!(snap.join("materials/a.json").is_file());
        assert!(!snap.join("scene.pkg").exists(), "生成物不进快照");
        // 快照里声明的版本号必须与目录名一致，回退后工作副本才对得上号
        assert_eq!(read_version(&snap), 1);
        assert_eq!(
            std::fs::read_to_string(snap.join(SNAPSHOT_META))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|m| m.get("note").cloned()),
            Some(json!("初版"))
        );
        // 版本号变了 → 旧包失效
        assert!(!dir.join("scene.pkg").exists());
        assert_eq!(r["packInvalidated"], json!(true));
    }

    #[test]
    fn snapshot_never_overwrites_existing_history() {
        let dir = versioned_project("snap-append", 1);
        snapshot_dir(&dir, "t", None).unwrap();
        assert_eq!(read_version(&dir), 2);

        // 把版本号手动拨回 1（等价于回退过），再改内容后冻结：
        // 第 1 版必须原封不动，新快照顺延到下一个没人用过的号
        std::fs::write(dir.join("scene.json"), r#"{"objects":[{"id":1}]}"#).unwrap();
        set_project_version(&dir, 1).unwrap();
        let r = snapshot_dir(&dir, "t", None).unwrap();
        assert_eq!(r["version"], json!(2), "第 1 版被占 → 顺延到 2");
        assert!(r["adjusted"].is_string(), "顺延要给出来源说明: {r}");
        assert_eq!(read_version(&dir), 3);
        assert!(!std::fs::read_to_string(dir.join(VERSION_DIR).join("1/scene.json"))
            .unwrap()
            .contains("id"));
    }

    #[test]
    fn snapshot_is_idempotent_when_content_already_frozen() {
        let dir = versioned_project("snap-idem", 1);
        snapshot_dir(&dir, "t", None).unwrap();
        // 回退到第 1 版后工作副本与快照逐字节一致 → 再冻结是空操作
        rollback_dir(&dir, "t", 1, true).unwrap();
        let r = snapshot_dir(&dir, "t", None).unwrap();
        assert_eq!(r["frozen"], json!(false), "{r}");
        assert_eq!(read_version(&dir), 1, "不该白白把版本号推走");
    }

    #[test]
    fn rollback_restores_exact_tree_and_drops_newer_files() {
        let dir = versioned_project("rollback", 1);
        snapshot_dir(&dir, "t", None).unwrap();

        // 第 2 版：改了内容、加了新文件
        std::fs::write(dir.join("scene.json"), r#"{"objects":[{"id":9}]}"#).unwrap();
        std::fs::write(dir.join("materials/new.json"), r#"{"passes":[]}"#).unwrap();
        std::fs::write(dir.join("scene.pkg"), b"PKGV0022-v2").unwrap();

        let r = rollback_dir(&dir, "t", 1, true).unwrap();
        assert_eq!(r["version"], json!(1));
        assert_eq!(read_version(&dir), 1);
        assert_eq!(
            std::fs::read_to_string(dir.join("scene.json")).unwrap(),
            r#"{"objects":[]}"#
        );
        assert!(!dir.join("materials/new.json").exists(), "新版多出来的文件要删掉");
        assert!(!dir.join("scene.pkg").exists(), "源变了包就过期");
        assert_eq!(r["packInvalidated"], json!(true));
    }

    #[test]
    fn rollback_refuses_to_discard_unfrozen_changes() {
        let dir = versioned_project("rollback-dirty", 1);
        snapshot_dir(&dir, "t", None).unwrap();
        std::fs::write(dir.join("scene.json"), r#"{"objects":[{"id":7}]}"#).unwrap();

        let err = rollback_dir(&dir, "t", 1, false).unwrap_err();
        assert!(err.contains("force"), "{err}");
        // 内容没被动过
        assert!(std::fs::read_to_string(dir.join("scene.json"))
            .unwrap()
            .contains("id"));

        rollback_dir(&dir, "t", 1, true).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("scene.json")).unwrap(),
            r#"{"objects":[]}"#
        );
    }

    #[test]
    fn rollback_reports_unknown_version() {
        let dir = versioned_project("rollback-missing", 1);
        let err = rollback_dir(&dir, "t", 7, false).unwrap_err();
        assert!(err.contains("第 7 版不存在"), "{err}");
    }

    #[test]
    fn versions_list_reports_current_and_history() {
        let dir = versioned_project("versions", 1);
        snapshot_dir(&dir, "t", Some("v1")).unwrap();
        snapshot_dir(&dir, "t", Some("v2")).unwrap();

        let out = versions_dir(&dir, "t").unwrap();
        assert_eq!(out["current"], json!(3));
        assert_eq!(out["count"], json!(2));
        let list = out["versions"].as_array().unwrap();
        assert_eq!(list[0]["version"], json!(1));
        assert_eq!(list[1]["version"], json!(2));
        assert_eq!(list[0]["note"], json!("v1"));
        assert!(list[0]["files"].as_u64().unwrap() >= 3);
        assert!(list[0]["bytes"].as_u64().unwrap() > 0);
    }

    /// 历史副本不能被当成普通工程内容：不进包、不算源文件
    #[test]
    fn version_history_is_excluded_from_pack_and_source_walk() {
        let dir = versioned_project("hist-excluded", 1);
        snapshot_dir(&dir, "t", None).unwrap();
        std::fs::write(dir.join("materials/bg.png"), sample_png()).unwrap();

        let mut files = Vec::new();
        collect_source_files(&dir, &dir, &mut files).unwrap();
        assert!(
            files.iter().all(|(rel, _)| !rel.starts_with(VERSION_DIR)),
            "源文件遍历不该进历史目录: {files:?}"
        );

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

    /// project_update 的「让位 / 回滚」：更新失败必须还能把旧版本放回来
    #[test]
    fn stage_aside_swaps_dir_and_restores_on_failure() {
        let root = tmpdir("stage-aside");
        let dir = root.join("item-1");
        let backup = root.join(".item-1.bak-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("scene.pkg"), b"old").unwrap();

        let moved = stage_aside(&dir, &backup).expect("让位应当成功");
        assert!(moved);
        assert!(!dir.exists(), "让位后原 id 必须空出来（否则导入会退到 -1 后缀）");
        assert_eq!(std::fs::read(backup.join("scene.pkg")).unwrap(), b"old");

        // 新版本写到一半就失败：清掉半成品再把旧的放回去
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("scene.pkg"), b"half-written").unwrap();
        restore_aside(&dir, &backup, moved);
        assert_eq!(std::fs::read(dir.join("scene.pkg")).unwrap(), b"old");
        assert!(!backup.exists(), "回滚后不该留下备份目录");

        // 库里本来没有这个条目时，不能凭空造出目录
        let missing = root.join("item-2");
        assert!(!stage_aside(&missing, &root.join(".item-2.bak-1")).unwrap());
        assert!(!missing.exists());

        // 上一次崩在让位途中留下的残壳，重试时要被清掉而不是叠加
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("stale"), b"junk").unwrap();
        assert!(stage_aside(&dir, &backup).unwrap());
        assert!(!backup.join("stale").exists(), "残壳应当被清掉");
        assert!(backup.join("scene.pkg").is_file());
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
        assert!(warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("还没转成 .tex")));
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
}
