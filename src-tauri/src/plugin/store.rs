//! 第三方插件：本地清单存储（热插拔）+ 市场数据源。
//!
//! # 插件是什么
//!
//! 第三方插件是**一份声明式清单**，不是可执行代码：
//!
//! ```text
//! <appData>/plugins/<id>/wem-plugin.json
//! ```
//!
//! ```json
//! {
//!   "id": "shadertoy",
//!   "name": "Shadertoy",
//!   "summary": "一句话说明",
//!   "description": "展开后的完整说明",
//!   "category": "创作工具",
//!   "icon": "✨",
//!   "author": "someone",
//!   "version": "1.0.0",
//!   "homepage": "https://github.com/someone/wem-shadertoy",
//!   "entry": { "type": "url", "url": "https://www.shadertoy.com/", "open": "external" }
//! }
//! ```
//!
//! 这条边界是刻意画的：从网上拉一个包进本机**执行**，与这个应用的信任模型不相容。
//! 声明式清单让「热插拔」退化成纯粹的「增删一个目录」—— 装、卸、手改、重扫都立刻
//! 生效，不需要重启应用，也不需要为第三方代码开任何权限。
//!
//! # 市场数据源
//!
//! 两条并集，**官方清单先出、GitHub 结果叠加**（前端合并；离线时只有官方清单）：
//! - 随包官方清单：前端 `src/lib/plugins.ts` 的 `THIRD_PARTY_PLUGINS`，安装时把
//!   条目原样写成本地清单（不下载，秒装）；
//! - GitHub：`topic:wem-plugin` 的仓库搜索（见 [`market_search`]）—— 仓库根放一份
//!   `wem-plugin.json` 就能被「从 GitHub 安装」拉下来落地。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use super::window::open_url_window;
use super::PluginError;

/// 第三方插件仓库打的 GitHub 话题：打上它，市场就能搜到
pub const GITHUB_TOPIC: &str = "wem-plugin";
/// 清单文件名（仓库根 / 本地插件目录同名）
pub const MANIFEST_FILE: &str = "wem-plugin.json";
/// 清单只是元数据：超过这个体积一定是拉错了东西（HTML 错误页、二进制包）
const MAX_MANIFEST_BYTES: usize = 128 * 1024;
/// 市场搜索单页条数
const MARKET_PAGE_SIZE: u32 = 30;
/// 搜索结果缓存时长。GitHub 匿名搜索限流 10 次/分钟，翻来覆去搜同一个词不该扣额度
const SEARCH_TTL: Duration = Duration::from_secs(90);
const USER_AGENT: &str = concat!("WallpaperEM/", env!("CARGO_PKG_VERSION"));

/// 清单协议版本。宿主只认「小于等于自己」的版本：
/// - 缺省按当前版本处理（老清单不用改）；
/// - 比宿主新 → 明确拒绝（`plugin-schema`），而不是按老规则猜着解释 —— 猜错的
///   后果比拒绝严重（把不认识的能力当成能用）。
pub const SCHEMA_VERSION: u32 = 1;
/// 宿主当前支持的**能力**（capabilities）白名单。清单声明了白名单之外的能力一律
/// 拒绝：能力是宿主给出的承诺，不是插件单方面声明就能生效的东西。
const SUPPORTED_CAPABILITIES: [&str; 3] = ["open-url", "open-window", "dsh-profile"];
/// **特权能力**：会往 DeepSeek Harness 的 profile 里装第三方插件包 —— 那些包是会被
/// dsh 真正执行的代码。界面必须显式提示并让用户确认，不能跟普通外链一个待遇。
const PRIVILEGED_CAPABILITIES: [&str; 1] = ["dsh-profile"];
/// 一个插件最多能往 profile 里塞多少个包（这份清单不是包管理器清单）
const MAX_PROFILE_PACKAGES: usize = 8;
/// 本应用版本（`minAppVersion` 的比对基准）
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 分类词表，与前端 `src/lib/plugins.ts` 的 `PLUGIN_CATEGORIES` **同源**：
/// 两边都改才算改完（词条本身走 `tr()`）。
const CATEGORIES: [&str; 4] = ["AI 助手", "壁纸资源", "创作工具", "其他"];
/// 内置插件占用的 id：第三方清单不许抢（否则「已安装」里会出现两个来源的同一个 id）
const RESERVED_IDS: [&str; 1] = ["dsh"];

// ---------------------------------------------------------------- 清单

#[derive(Debug, Deserialize)]
struct RawManifest {
    /// 可选：缺省时用目录名 / 仓库名兜底
    id: Option<String>,
    /// 必填，但**不交给 serde 报缺** —— 缺名字要报 `plugin-name`（作者知道该改什么），
    /// 与作者自查脚本保持同一个错误码
    name: Option<String>,
    summary: Option<String>,
    description: Option<String>,
    category: Option<String>,
    icon: Option<String>,
    author: Option<String>,
    version: Option<String>,
    homepage: Option<String>,
    /// 规范形状：入口描述
    entry: Option<RawEntry>,
    /// 容错：把入口直接平铺在顶层（`url` / `open` / `kind` / `packages`）也认 ——
    /// 前端随包官方清单里的条目就是这个形状，不必先转成 `entry: {…}` 再发过来
    url: Option<String>,
    open: Option<String>,
    kind: Option<String>,
    packages: Option<Vec<String>>,
    /// 清单协议版本，缺省 = 当前版本
    #[serde(rename = "schemaVersion")]
    schema_version: Option<u32>,
    /// 声明需要宿主具备的能力；缺省 = 由 `entry.open` 推导
    capabilities: Option<Vec<String>>,
    /// 要求的最低应用版本（三段数字，可带 `v` 前缀与 `-rc.1` 这类后缀）
    #[serde(rename = "minAppVersion")]
    min_app_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawEntry {
    /// `url`（缺省，打开一个地址）或 `dsh`（往 dsh profile 装包并打开 dsh 界面）
    #[serde(rename = "type")]
    kind: Option<String>,
    /// `url` 型的入口地址；`dsh` 型不需要
    url: Option<String>,
    /// external（默认，系统浏览器）| window（应用内窗口）——只有 `url` 型用到
    open: Option<String>,
    /// `dsh` 型要装进 profile 的 npm 包名（可带版本/标签）
    packages: Option<Vec<String>>,
}

/// 校验通过的清单：字段全部规整过，可以直接落盘/回给前端。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Manifest {
    schema_version: u32,
    /// 入口类型：`url` | `dsh`
    kind: String,
    id: String,
    name: String,
    summary: String,
    description: String,
    category: String,
    icon: String,
    author: String,
    version: String,
    homepage: String,
    url: String,
    open: String,
    capabilities: Vec<String>,
    /// `dsh` 型要装的包（`url` 型恒为空）
    packages: Vec<String>,
    min_app_version: String,
    /// 当前应用版本是否满足 `minAppVersion`（不满足仍可安装，但打不开）
    compatible: bool,
}

fn text(v: Option<String>, fallback: &str) -> String {
    v.map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

/// 原始分类 → 本应用分类词表。
///
/// 清单里写中文分类名（四个词表之一）直接用；GitHub 仓库给的是英文 topic，按词
/// 映射 —— 用**整词**匹配而不是 `contains`：「detail」里含 "ai"，`contains` 会把
/// 它划进 AI 助手。
fn category_of(raw: Option<&str>) -> &'static str {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return "其他";
    };
    if let Some(hit) = CATEGORIES.iter().find(|c| **c == s) {
        return hit;
    }
    let lower = s.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let has = |set: &[&str]| words.iter().any(|w| set.contains(w));
    if has(&["ai", "agent", "dsh", "llm", "mcp", "copilot"]) {
        "AI 助手"
    } else if has(&[
        "wallpaper",
        "asset",
        "assets",
        "resource",
        "resources",
        "image",
        "images",
    ]) {
        "壁纸资源"
    } else if has(&["shader", "scene", "effect", "editor", "tool", "tools", "we"]) {
        "创作工具"
    } else {
        "其他"
    }
}

/// 插件 id：小写字母/数字/`-`/`_`，首字符是字母或数字，最长 48。
///
/// 中文名（GitHub 仓库名常见）剥完会变空 —— 这时退化成内容哈希，保证**同一个名字
/// 永远得到同一个 id**（否则每次安装都会新建一个目录）。
fn slug(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if !out.is_empty()
        && out
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
    {
        return Some(out);
    }
    if raw.trim().is_empty() {
        return None;
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    raw.trim().hash(&mut h);
    Some(format!("p-{:016x}", h.finish()))
}

/// 三段数字版本：缺段补 0，`v` 前缀与 `-rc.1` / `+build` 后缀忽略。
/// 解析不了返回 None（由调用方决定是拒绝还是放过）。
fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let core = s.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next().unwrap_or(core);
    let mut nums = [0u32; 3];
    let mut seen = false;
    for (i, part) in core.split('.').enumerate() {
        if i >= 3 {
            break;
        }
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        nums[i] = digits.parse().ok()?;
        seen = true;
    }
    if seen {
        Some((nums[0], nums[1], nums[2]))
    } else {
        None
    }
}

/// 当前版本是否 ≥ 要求版本。任一侧解析不了就**放行** —— 宁可少拦一次，也不误伤
/// 一个本来能用的插件（真正要拦的 `minAppVersion` 格式在 validate 里已经拒了）。
fn version_at_least(current: &str, required: &str) -> bool {
    match (parse_version(current), parse_version(required)) {
        (Some(c), Some(r)) => c >= r,
        _ => true,
    }
}

/// 入口隐含需要的能力：`dsh` 型要 `dsh-profile`；开应用内窗口要 `open-window`；
/// 其余都是 `open-url`
fn capability_for(kind: &str, open: &str) -> &'static str {
    if kind == "dsh" {
        "dsh-profile"
    } else if open == "window" {
        "open-window"
    } else {
        "open-url"
    }
}

/// npm 包名（不含版本段）：小写字母/数字/`-`/`_`/`.`，不许以 `.`/`_` 开头，
/// 不许出现 `..`（那是路径，不是包名）
fn valid_npm_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 214
        && !n.starts_with('.')
        && !n.starts_with('_')
        && !n.contains("..")
        && n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

/// 包规格：`pkg`、`@scope/pkg`、`pkg@^1.2.0`、`@scope/pkg@next`。
///
/// 刻意**只放行注册表包名 + 版本范围/标签**：
/// - 开头是 `-` 的会被 pnpm 当 flag（参数注入）；
/// - `file:` / `link:` / `git+…` / `../x` 等于让清单决定「从哪拉代码」——那超出这个
///   能力的边界，真要装本机包应当由用户自己跑 `dsh plugin`。
fn valid_package_spec(raw: &str) -> bool {
    let s = raw.trim();
    if s.is_empty()
        || s.len() > 214
        || s.starts_with('-')
        || s.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return false;
    }
    // 版本分隔符从右往左找 `@`：作用域名本身以 `@` 开头，不能当分隔符
    let (name, version) = match s.rfind('@') {
        None | Some(0) => (s, ""),
        Some(i) => (&s[..i], &s[i + 1..]),
    };
    if !version.is_empty() {
        if version.contains(':') || version.contains('/') || version.starts_with('.') {
            return false;
        }
        let ok = version.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '-' | '_' | '.' | '+' | '^' | '~' | '*' | '<' | '>' | '=' | '|'
                )
        });
        if !ok {
            return false;
        }
    }
    if let Some(rest) = name.strip_prefix('@') {
        let mut parts = rest.splitn(2, '/');
        let scope = parts.next().unwrap_or("");
        let pkg = parts.next().unwrap_or("");
        valid_npm_name(scope) && valid_npm_name(pkg)
    } else {
        valid_npm_name(name)
    }
}

/// 能力归一与校验：白名单之外一律拒绝；声明必须覆盖入口实际需要的能力
/// （`open: window` 却只声明 `open-url` 是自相矛盾的清单）。
fn normalize_capabilities(
    raw: Option<Vec<String>>,
    implied: &str,
) -> Result<Vec<String>, PluginError> {
    let Some(list) = raw else {
        return Ok(vec![implied.to_string()]);
    };
    let mut out: Vec<String> = Vec::new();
    for c in list {
        let c = c.trim().to_lowercase();
        if c.is_empty() {
            continue;
        }
        if !SUPPORTED_CAPABILITIES.contains(&c.as_str()) {
            return Err(PluginError::new("plugin-capability", c));
        }
        if !out.contains(&c) {
            out.push(c);
        }
    }
    if !out.iter().any(|c| c == implied) {
        return Err(PluginError::new("plugin-capability", implied));
    }
    // `dsh` 型只有一个入口，多声明别的能力没有意义（将来要组合入口时再放开）
    if implied == "dsh-profile" && out.len() > 1 {
        return Err(PluginError::new("plugin-capability", out.join(",")));
    }
    Ok(out)
}

fn validate(v: Value, id_hint: &str) -> Result<Manifest, PluginError> {
    let raw: RawManifest =
        serde_json::from_value(v).map_err(|e| PluginError::new("plugin-manifest", e))?;
    // 两种形状归一：`entry: { … }`（规范，落盘也是这个）或顶层平铺（容错）
    let entry = match raw.entry {
        Some(e) => e,
        None if raw.url.is_some() || raw.kind.is_some() || raw.packages.is_some() => RawEntry {
            kind: raw.kind.clone(),
            url: raw.url.clone(),
            open: raw.open.clone(),
            packages: raw.packages.clone(),
        },
        None => return Err(PluginError::code("plugin-manifest")),
    };
    let name = text(raw.name, "");
    if name.is_empty() || name.chars().count() > 60 {
        return Err(PluginError::code("plugin-name"));
    }
    let kind = match entry
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
    {
        None | Some("url") => "url",
        Some("dsh") => "dsh",
        Some(_) => return Err(PluginError::code("plugin-entry-type")),
    };
    // `dsh` 型：入口是「往 dsh profile 装这些包」，不需要 url
    let (url, open, packages) = if kind == "dsh" {
        let packages: Vec<String> = entry
            .packages
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        if packages.is_empty() || packages.len() > MAX_PROFILE_PACKAGES {
            return Err(PluginError::new(
                "plugin-packages",
                packages.len().to_string(),
            ));
        }
        for p in &packages {
            if !valid_package_spec(p) {
                return Err(PluginError::new("plugin-packages", p));
            }
        }
        (String::new(), "window".to_string(), packages)
    } else {
        // 只认 http(s)：file:// 能读本机任意文件，javascript: 在 webview 里就是执行代码
        let url = entry.url.as_deref().unwrap_or("").trim().to_string();
        if !url.starts_with("https://") && !url.starts_with("http://") {
            return Err(PluginError::code("plugin-url"));
        }
        let open = match entry.open.as_deref() {
            None | Some("") | Some("external") => "external",
            Some("window") => "window",
            Some(_) => return Err(PluginError::code("plugin-open")),
        };
        (url, open.to_string(), Vec::new())
    };
    let id_raw = text(raw.id, id_hint);
    let id = slug(&id_raw).ok_or_else(|| PluginError::code("plugin-id"))?;
    if RESERVED_IDS.contains(&id.as_str()) {
        return Err(PluginError::code("plugin-id-reserved"));
    }
    // 协议版本：只认 ≤ 宿主版本；比宿主新就拒绝，别按老规则猜着解释
    let schema_version = raw.schema_version.unwrap_or(SCHEMA_VERSION);
    if schema_version == 0 || schema_version > SCHEMA_VERSION {
        return Err(PluginError::new(
            "plugin-schema",
            schema_version.to_string(),
        ));
    }
    let capabilities = normalize_capabilities(raw.capabilities, capability_for(kind, &open))?;
    // 最低应用版本：格式错是作者笔误，直接拒；格式对但本机版本不够 → 可装不可开
    let min_app_version = text(raw.min_app_version, "");
    if !min_app_version.is_empty() && parse_version(&min_app_version).is_none() {
        return Err(PluginError::new("plugin-app-version", &min_app_version));
    }
    let compatible = min_app_version.is_empty() || version_at_least(APP_VERSION, &min_app_version);
    Ok(Manifest {
        schema_version,
        kind: kind.to_string(),
        category: category_of(raw.category.as_deref()).to_string(),
        id,
        name,
        summary: text(raw.summary, ""),
        description: text(raw.description, ""),
        icon: text(raw.icon, "🧩"),
        author: text(raw.author, ""),
        version: text(raw.version, ""),
        homepage: text(raw.homepage, ""),
        url,
        open,
        capabilities,
        packages,
        min_app_version,
        compatible,
    })
}

/// 落盘的入口描述：`url` 型写地址与打开方式，`dsh` 型写要装的包
fn entry_json(m: &Manifest) -> Value {
    if m.kind == "dsh" {
        json!({ "type": "dsh", "packages": m.packages })
    } else {
        json!({ "type": "url", "url": m.url, "open": m.open })
    }
}

/// 清单 → 落盘 JSON（键序固定，人手看目录时也是这个顺序）
fn manifest_json(m: &Manifest) -> Value {
    let mut out = json!({
        "schemaVersion": m.schema_version,
        "id": m.id,
        "name": m.name,
        "summary": m.summary,
        "description": m.description,
        "category": m.category,
        "icon": m.icon,
        "author": m.author,
        "version": m.version,
        "homepage": m.homepage,
        "entry": entry_json(m),
        "capabilities": m.capabilities,
    });
    if !m.min_app_version.is_empty() {
        out["minAppVersion"] = json!(m.min_app_version);
    }
    out
}

/// 清单 → 前端条目（与内置/官方清单同一个形状，页面不用分两套渲染）
fn entry_value(m: &Manifest, dir: Option<&Path>) -> Value {
    json!({
        "id": m.id,
        "name": m.name,
        "summary": m.summary,
        "description": m.description,
        "category": m.category,
        "icon": m.icon,
        "author": m.author,
        "version": m.version,
        "homepage": m.homepage,
        "url": m.url,
        "open": m.open,
        "kind": m.kind,
        "packages": m.packages,
        "source": "third",
        "installed": dir.is_some(),
        "market": "local",
        "schemaVersion": m.schema_version,
        "capabilities": m.capabilities,
        "minAppVersion": m.min_app_version,
        "compatible": m.compatible,
        // 特权判定放在宿主侧：界面与协议文档都引用这一个真相
        "privileged": m
            .capabilities
            .iter()
            .any(|c| PRIVILEGED_CAPABILITIES.contains(&c.as_str())),
        "dir": dir.map(|d| d.to_string_lossy().to_string()),
    })
}

// ---------------------------------------------------------------- 目录读写（纯函数，便于测试）

/// 列出目录里所有合法清单。非法/半截的目录**跳过并记日志** —— 用户手放错一个
/// 文件不该让整页报错。
fn list_in(root: &Path) -> Result<Vec<Value>, PluginError> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(out);
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let file = dir.join(MANIFEST_FILE);
        if !file.is_file() {
            continue;
        }
        match read_manifest(&dir) {
            Some(m) => out.push(entry_value(&m, Some(&dir))),
            None => tracing::warn!("插件清单缺失或校验未通过，已跳过: {}", file.display()),
        }
    }
    out.sort_by(|a, b| {
        let key = |v: &Value| v["name"].as_str().unwrap_or("").to_lowercase();
        key(a).cmp(&key(b))
    });
    Ok(out)
}

/// 读一份已安装清单（不存在/非法返回 None）
fn read_manifest(dir: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(dir.join(MANIFEST_FILE)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let hint = dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    validate(v, &hint).ok()
}

/// 来源指纹：id 冲突判定用。主页优先（同一插件在官方清单与 GitHub 里主页通常相同），
/// 其次是入口地址。
fn source_key(m: &Manifest) -> String {
    let norm = |s: &str| s.trim().trim_end_matches('/').to_lowercase();
    if m.homepage.is_empty() {
        norm(&m.url)
    } else {
        norm(&m.homepage)
    }
}

fn write_in(root: &Path, v: Value, id_hint: &str) -> Result<Value, PluginError> {
    let m = validate(v, id_hint)?;
    let dir = root.join(&m.id);
    // id 冲突规则：同一个 id 已经被**别的来源**占用时拒绝覆盖。否则两个作者的同名
    // 插件会互相盖掉，用户还看不出自己装的是哪一个。同来源 = 重装/升级，允许原地覆盖。
    if let Some(existing) = read_manifest(&dir) {
        if source_key(&existing) != source_key(&m) {
            return Err(PluginError::new(
                "plugin-id-conflict",
                format!("{} != {}", source_key(&existing), source_key(&m)),
            ));
        }
    }
    std::fs::create_dir_all(&dir).map_err(|e| PluginError::new("plugin-write", e))?;
    let body = serde_json::to_string_pretty(&manifest_json(&m))
        .map_err(|e| PluginError::new("plugin-write", e))?;
    std::fs::write(dir.join(MANIFEST_FILE), format!("{body}\n"))
        .map_err(|e| PluginError::new("plugin-write", e))?;
    tracing::info!("插件已安装: {} → {}", m.id, dir.display());
    Ok(entry_value(&m, Some(&dir)))
}

fn remove_in(root: &Path, id: &str) -> Result<(), PluginError> {
    let id = slug(id).ok_or_else(|| PluginError::code("plugin-id"))?;
    if RESERVED_IDS.contains(&id.as_str()) {
        return Err(PluginError::code("plugin-id-reserved"));
    }
    let dir = root.join(&id);
    // 只删「看起来真是我们装的」目录：没有清单文件就不动它
    if !dir.join(MANIFEST_FILE).is_file() {
        return Err(PluginError::code("plugin-not-installed"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| PluginError::new("plugin-remove", e))?;
    tracing::info!("插件已卸载: {id}");
    Ok(())
}

// ---------------------------------------------------------------- 应用数据目录

pub(crate) fn plugins_root(app: &AppHandle) -> Result<PathBuf, PluginError> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| PluginError::new("plugin-dir", e))?
        .join("plugins");
    std::fs::create_dir_all(&dir).map_err(|e| PluginError::new("plugin-dir", e))?;
    Ok(dir)
}

// ---------------------------------------------------------------- 网络

fn http_client(app: &AppHandle) -> Result<reqwest::Client, PluginError> {
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(20));
    // 与下载/工坊同源：用户配了代理就走代理（GitHub 在部分地区必须走）
    if let Some(p) = crate::download::read_proxy(app) {
        if let Ok(proxy) = reqwest::Proxy::all(&p) {
            builder = builder.proxy(proxy);
        }
    }
    builder
        .build()
        .map_err(|e| PluginError::new("plugin-http", e))
}

/// 限量读取响应体：清单是元数据，几十 KB 顶天，剩下的直接判错而不是读完再丢。
async fn read_capped(mut resp: reqwest::Response) -> Result<Vec<u8>, PluginError> {
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_MANIFEST_BYTES {
            return Err(PluginError::code("plugin-too-large"));
        }
    }
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| PluginError::new("plugin-fetch", e))?
    {
        if buf.len() + chunk.len() > MAX_MANIFEST_BYTES {
            return Err(PluginError::code("plugin-too-large"));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// 从清单地址猜 id：`…/owner/repo/HEAD/wem-plugin.json` → `repo`。
fn id_hint_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let is_ref = |s: &str| {
        s == "HEAD"
            || s == "main"
            || s == "master"
            || (s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()))
    };
    // 从后往前：末段是文件名，再往前通常是 ref（HEAD/main/<sha>），再往前才是仓库名
    let mut idx = segs.len().saturating_sub(2);
    if segs.get(idx).copied().is_some_and(is_ref) {
        idx = idx.saturating_sub(1);
    }
    segs.get(idx).map(|s| s.to_string()).unwrap_or_default()
}

// ---------------------------------------------------------------- 市场（GitHub）

#[derive(Debug, Deserialize)]
struct GhRepo {
    full_name: String,
    name: String,
    description: Option<String>,
    html_url: String,
    stargazers_count: u64,
    updated_at: Option<String>,
    archived: Option<bool>,
    topics: Option<Vec<String>>,
    owner: GhOwner,
}

#[derive(Debug, Deserialize)]
struct GhOwner {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GhSearch {
    total_count: u64,
    items: Vec<GhRepo>,
}

/// 仓库 → 市场条目。
///
/// `manifestUrl` 用 `HEAD` 而不是默认分支名：省一次 API 调用，也不需要知道对方
/// 是 main 还是 master。**能不能装**要到真去拉清单才知道，所以这里不预判 ——
/// 安装失败会给出明确错误（缺清单文件/不是合法 JSON）。
fn market_entry(r: &GhRepo) -> Value {
    // 仓库的 topics 是按字母序排的（第一个可能是 "csharp" 这种），只取一个当分类
    // 会让绝大多数结果掉进「其他」—— 整组拼起来交给整词匹配。
    let topics = r.topics.clone().unwrap_or_default().join(" ");
    let category = category_of(Some(topics.as_str()).filter(|t| !t.is_empty()));
    json!({
        "id": format!("github:{}", r.full_name),
        "name": r.name,
        "summary": r.description.clone().unwrap_or_default(),
        "description": r.description.clone().unwrap_or_default(),
        "category": category,
        "icon": "🧩",
        "author": r.owner.login,
        "version": "",
        "homepage": r.html_url,
        "url": r.html_url,
        "open": "external",
        "source": "third",
        "installed": false,
        "market": "github",
        "stars": r.stargazers_count,
        "updatedAt": r.updated_at,
        "archived": r.archived.unwrap_or(false),
        "manifestUrl": format!(
            "https://raw.githubusercontent.com/{}/HEAD/{MANIFEST_FILE}",
            r.full_name
        ),
    })
}

/// 解析 GitHub 搜索响应（纯函数：网络与解析分开，解析可单测）
fn parse_search(bytes: &[u8]) -> Result<(u64, Vec<Value>), PluginError> {
    let parsed: GhSearch =
        serde_json::from_slice(bytes).map_err(|e| PluginError::new("plugin-parse", e))?;
    Ok((
        parsed.total_count,
        parsed.items.iter().map(market_entry).collect(),
    ))
}

/// 搜索词的组装：话题是**并集的基础**，关键词是收窄。
fn build_query(topic: &str, query: &str) -> String {
    let q = query.trim();
    if q.is_empty() {
        format!("topic:{topic}")
    } else {
        format!("topic:{topic} {q}")
    }
}

/// GitHub 搜索排序参数：`(sort, order)`。名称排序 GitHub 不支持 —— 取默认相关度，
/// 由前端在合并后的列表里按名称排。
fn search_order(sort: &str) -> Option<(&'static str, &'static str)> {
    match sort {
        "stars" => Some(("stars", "desc")),
        "updated" => Some(("updated", "desc")),
        _ => None,
    }
}

/// 上一次搜索的缓存：(查询键 → (取回时刻, 载荷))
static SEARCH_CACHE: Mutex<Option<(String, Instant, Value)>> = Mutex::new(None);

/// 市场搜索：话题 `wem-plugin` + 关键词，返回归一化条目。
///
/// 失败**不报错**，而是回一个带 `error` 的载荷 —— 市场页离线/限流时应当继续显示
/// 官方清单，而不是弹一个红色错误把整个页面打空。
#[tauri::command]
pub async fn plugin_market_search(
    app: AppHandle,
    query: String,
    sort: String,
    topic: Option<String>,
) -> Value {
    let topic = topic
        .map(|t| t.trim().to_string())
        .filter(|t| {
            !t.is_empty()
                && t.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .unwrap_or_else(|| GITHUB_TOPIC.to_string());
    let q = build_query(&topic, &query);
    let key = format!("{q}|{sort}");

    if let Ok(guard) = SEARCH_CACHE.lock() {
        if let Some((k, at, payload)) = guard.as_ref() {
            if *k == key && at.elapsed() < SEARCH_TTL {
                let mut out = payload.clone();
                out["cached"] = json!(true);
                return out;
            }
        }
    }

    let client = match http_client(&app) {
        Ok(c) => c,
        Err(e) => return search_failed(&topic, &e.code, &e.detail),
    };
    let mut params: Vec<(&str, String)> =
        vec![("q", q.clone()), ("per_page", MARKET_PAGE_SIZE.to_string())];
    if let Some((sort, order)) = search_order(&sort) {
        params.push(("sort", sort.to_string()));
        params.push(("order", order.to_string()));
    }

    let resp = match client
        .get("https://api.github.com/search/repositories")
        .header("Accept", "application/vnd.github+json")
        .query(&params)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return search_failed(&topic, "plugin-network", &e.to_string()),
    };

    let status = resp.status();
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        // 匿名搜索限流 10 次/分钟；把重置时间带回去，界面能说清「多久之后能再搜」
        let reset = resp
            .headers()
            .get("x-ratelimit-reset")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok());
        return json!({
            "items": [],
            "totalCount": 0,
            "error": "plugin-rate-limited",
            "errorDetail": format!("HTTP {status}"),
            "topic": topic,
            "rateLimitReset": reset,
            "cached": false,
        });
    }
    if !status.is_success() {
        return search_failed(&topic, "plugin-http-status", &format!("HTTP {status}"));
    }

    let bytes = match read_capped(resp).await {
        Ok(b) => b,
        Err(e) => return search_failed(&topic, &e.code, &e.detail),
    };
    match parse_search(&bytes) {
        Ok((total, items)) => {
            let payload = json!({
                "items": items,
                "totalCount": total,
                "error": Value::Null,
                "errorDetail": Value::Null,
                "topic": topic,
                "rateLimitReset": Value::Null,
                "cached": false,
            });
            if let Ok(mut guard) = SEARCH_CACHE.lock() {
                *guard = Some((key, Instant::now(), payload.clone()));
            }
            payload
        }
        Err(e) => search_failed(&topic, &e.code, &e.detail),
    }
}

fn search_failed(topic: &str, code: &str, detail: &str) -> Value {
    tracing::warn!("插件市场搜索失败（{code}）: {detail}");
    json!({
        "items": [],
        "totalCount": 0,
        "error": code,
        "errorDetail": detail,
        "topic": topic,
        "rateLimitReset": Value::Null,
        "cached": false,
    })
}

// ---------------------------------------------------------------- 命令

/// 已安装的第三方插件（热插拔：这就是「当前磁盘上的样子」，装完/卸完立刻反映）
#[tauri::command]
pub fn plugin_installed(app: AppHandle) -> Value {
    match plugins_root(&app).and_then(|root| list_in(&root).map(|items| (root, items))) {
        Ok((root, items)) => json!({
            "items": items,
            "dir": root.to_string_lossy(),
            "count": items.len(),
        }),
        Err(e) => json!({ "items": [], "dir": "", "count": 0, "error": e.code }),
    }
}

/// 从清单地址安装（GitHub raw / 任意 http(s)）
#[tauri::command]
pub async fn plugin_install_url(app: AppHandle, url: String) -> Result<Value, PluginError> {
    let url = url.trim().to_string();
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(PluginError::code("plugin-url"));
    }
    let client = http_client(&app)?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| PluginError::new("plugin-network", e))?;
    if !resp.status().is_success() {
        return Err(PluginError::new(
            "plugin-http-status",
            format!("HTTP {}", resp.status()),
        ));
    }
    let bytes = read_capped(resp).await?;
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|e| PluginError::new("plugin-parse", e))?;
    let root = plugins_root(&app)?;
    write_in(&root, v, &id_hint_from_url(&url))
}

/// 卸载（只删插件目录，别的一概不碰）
#[tauri::command]
pub fn plugin_uninstall(app: AppHandle, id: String) -> Result<Value, PluginError> {
    let root = plugins_root(&app)?;
    remove_in(&root, &id)?;
    Ok(json!({ "ok": true, "id": id }))
}

/// 打开一个 `dsh` 型第三方插件：把它的包装进干净 profile，再打开 dsh 窗口。
///
/// 装包要联网、要 pnpm、还要等 profile 写锁（dsh 自己就等最多 2 分钟），所以整个过程
/// 放在 spawn_blocking 里，超时给到 5 分钟；界面在这期间显示「安装中…」。
#[tauri::command]
pub async fn plugin_dsh_open(app: AppHandle, id: String) -> Result<Value, PluginError> {
    let root = plugins_root(&app)?;
    let dir = root.join(slug(&id).ok_or_else(|| PluginError::code("plugin-id"))?);
    let m = read_manifest(&dir).ok_or_else(|| PluginError::code("plugin-not-installed"))?;
    if m.kind != "dsh" {
        return Err(PluginError::code("plugin-entry-type"));
    }
    if !m.compatible {
        return Err(PluginError::new("plugin-app-version", &m.min_app_version));
    }

    // 缺的包先装进 profile。**装完必须重启**：干净 profile 没开 HMR，组合包清单
    // 是 dsh 的 plugin-manager 在装完后写进 profile package.json 的，只有下次启动
    // 才会被加载 —— 所以先收掉在跑的实例与它的窗口，稍后重新开。
    let missing = super::dsh::missing_packages(&m.packages);
    if !missing.is_empty() {
        super::dsh::stop_and_close(&app);
        let pkgs = missing.clone();
        tauri::async_runtime::spawn_blocking(move || super::dsh::install_profile_packages(&pkgs))
            .await
            .map_err(|e| PluginError::new("plugin-pnpm", e))??;
    }

    let dsh = super::dsh::ensure_running(&app).await?;
    Ok(json!({
        "packages": m.packages,
        "installedNow": missing,
        "url": dsh.get("url").cloned().unwrap_or(Value::Null),
        "profileDir": super::dsh::profile_dir().to_string_lossy(),
    }))
}

/// 在文件管理器里打开插件目录：手放一个清单文件夹进去 = 装好，重扫即可
#[tauri::command]
pub fn plugin_open_dir(app: AppHandle) -> Result<Value, PluginError> {
    let root = plugins_root(&app)?;
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(root.display().to_string(), None::<&str>)
        .map_err(|e| PluginError::new("plugin-open-dir", e))?;
    Ok(json!({ "dir": root.to_string_lossy() }))
}

/// 把第三方插件开成应用内窗口（清单里 `entry.open = "window"` 的走这里）
#[tauri::command]
pub fn plugin_open_window(
    app: AppHandle,
    id: String,
    url: String,
    name: String,
) -> Result<Value, PluginError> {
    let url = url.trim().to_string();
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(PluginError::code("plugin-url"));
    }
    let id = slug(&id).unwrap_or_else(|| "plugin".into());
    let title = text(Some(name), "Plugin");
    let label = format!("plugin-{id}");
    // 同步命令跑在主线程：开窗必须在这里做（与 dsh 的开窗路径同一个实现）
    open_url_window(&app, &label, &url, &title, || {})
        .map_err(|e| PluginError::new("plugin-window", e))?;
    Ok(json!({ "label": label, "url": url }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("we-store-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn good() -> Value {
        json!({
            "name": "Shadertoy",
            "summary": "全屏着色器",
            "category": "创作工具",
            "icon": "✨",
            "author": "Shadertoy",
            "version": "1.0.0",
            "homepage": "https://github.com/me/wem-shadertoy",
            "entry": { "type": "url", "url": "https://www.shadertoy.com/", "open": "external" }
        })
    }

    #[test]
    fn slug_is_stable_and_safe() {
        assert_eq!(slug("My Plugin!").as_deref(), Some("my-plugin"));
        assert_eq!(slug("wem-shadertoy").as_deref(), Some("wem-shadertoy"));
        assert_eq!(slug("../../etc/passwd").as_deref(), Some("etc-passwd"));
        assert_eq!(slug("   "), None);
        // 中文名：退化成内容哈希，但同一个名字必须得到同一个 id
        let a = slug("壁纸工具").unwrap();
        let b = slug("壁纸工具").unwrap();
        assert_eq!(a, b);
        assert!(a.starts_with("p-"));
    }

    #[test]
    fn category_maps_topics_without_substring_traps() {
        assert_eq!(category_of(Some("创作工具")), "创作工具");
        assert_eq!(category_of(Some("wallpaper-engine")), "壁纸资源");
        assert_eq!(category_of(Some("ai")), "AI 助手");
        assert_eq!(category_of(Some("detail-viewer")), "其他");
        assert_eq!(category_of(None), "其他");
    }

    #[test]
    fn validate_rejects_dangerous_entries() {
        let mut v = good();
        v["entry"]["url"] = json!("file:///etc/passwd");
        assert_eq!(validate(v, "x").unwrap_err().code, "plugin-url");

        let mut v = good();
        v["entry"]["url"] = json!("javascript:alert(1)");
        assert_eq!(validate(v, "x").unwrap_err().code, "plugin-url");

        let mut v = good();
        v["entry"]["type"] = json!("shell");
        assert_eq!(validate(v, "x").unwrap_err().code, "plugin-entry-type");

        let mut v = good();
        v["entry"]["open"] = json!("nowhere");
        assert_eq!(validate(v, "x").unwrap_err().code, "plugin-open");

        let mut v = good();
        v["id"] = json!("dsh");
        assert_eq!(validate(v, "x").unwrap_err().code, "plugin-id-reserved");

        let mut v = good();
        v["name"] = json!("");
        assert_eq!(validate(v, "x").unwrap_err().code, "plugin-name");
    }

    #[test]
    fn accepts_top_level_url_shape_and_normalizes_it() {
        // 前端「官方清单」条目把入口写在顶层（url/open），清单规范是 entry 里 —— 两种都要认
        let v = json!({
            "id": "shadertoy",
            "name": "Shadertoy",
            "summary": "全屏着色器",
            "category": "创作工具",
            "icon": "✨",
            "author": "Shadertoy",
            "url": "https://www.shadertoy.com/",
            "open": "external",
        });
        let m = validate(v, "fallback").unwrap();
        assert_eq!(m.id, "shadertoy");
        assert_eq!(m.url, "https://www.shadertoy.com/");
        assert_eq!(m.open, "external");
        // 落盘一律写成规范形状：entry 里带 type/url/open
        let on_disk = manifest_json(&m);
        assert_eq!(on_disk["entry"]["type"], json!("url"));
        assert_eq!(on_disk["entry"]["open"], json!("external"));
        assert!(on_disk.get("url").is_none());

        // 两处都没有入口 → 报清单格式错
        let bad = json!({ "name": "x" });
        assert_eq!(validate(bad, "x").unwrap_err().code, "plugin-manifest");
    }

    #[test]
    fn version_parsing_and_comparison() {
        assert_eq!(parse_version("2.1.0"), Some((2, 1, 0)));
        assert_eq!(parse_version("v2.1"), Some((2, 1, 0)));
        assert_eq!(parse_version("2.1.0-rc.1"), Some((2, 1, 0)));
        assert_eq!(parse_version("2.1.0+build.7"), Some((2, 1, 0)));
        assert_eq!(parse_version("nope"), None);
        assert_eq!(parse_version(""), None);
        assert!(version_at_least("2.1.0", "2.1.0"));
        assert!(version_at_least("2.2.0", "2.1.9"));
        assert!(!version_at_least("2.0.9", "2.1"));
        assert!(version_at_least("2.0.0", "abc"), "解析不了的要求版本不拦人");
    }

    #[test]
    fn protocol_rules_are_enforced() {
        // schemaVersion 比宿主新 → 拒绝，而不是按老规则猜
        let mut v = good();
        v["schemaVersion"] = json!(SCHEMA_VERSION + 1);
        assert_eq!(validate(v, "x").unwrap_err().code, "plugin-schema");

        let mut v = good();
        v["schemaVersion"] = json!(0);
        assert_eq!(validate(v, "x").unwrap_err().code, "plugin-schema");

        // 白名单之外的capability → 拒绝
        let mut v = good();
        v["capabilities"] = json!(["open-url", "run-code"]);
        assert_eq!(validate(v, "x").unwrap_err().code, "plugin-capability");

        // open: window 却只声明 open-url → 自相矛盾
        let mut v = good();
        v["entry"]["open"] = json!("window");
        v["capabilities"] = json!(["open-url"]);
        assert_eq!(validate(v, "x").unwrap_err().code, "plugin-capability");

        // 不声明 → 由 entry.open 推导
        let m = validate(good(), "x").unwrap();
        assert_eq!(m.capabilities, vec!["open-url".to_string()]);
        let mut v = good();
        v["entry"]["open"] = json!("window");
        let m = validate(v, "x").unwrap();
        assert_eq!(m.capabilities, vec!["open-window".to_string()]);
        assert_eq!(m.schema_version, SCHEMA_VERSION);

        // minAppVersion：格式错拒绝；版本比本机高 → 可安装但 compatible=false
        let mut v = good();
        v["minAppVersion"] = json!("not-a-version");
        assert_eq!(validate(v, "x").unwrap_err().code, "plugin-app-version");

        let mut v = good();
        v["minAppVersion"] = json!("99.0.0");
        let m = validate(v, "x").unwrap();
        assert!(!m.compatible);
        assert_eq!(m.min_app_version, "99.0.0");

        let mut v = good();
        v["minAppVersion"] = json!(APP_VERSION);
        assert!(validate(v, "x").unwrap().compatible);
    }

    /// 落盘的键必须与 docs/wem-plugin.schema.json 声明的属性对得上 —— 否则照文档写的
    /// 插件会被宿主写出一份协议外的清单，作者与宿主从此各说各话。
    #[test]
    fn on_disk_manifest_matches_the_documented_schema_shape() {
        let mut v = good();
        v["schemaVersion"] = json!(SCHEMA_VERSION);
        v["minAppVersion"] = json!("2.0.0");
        v["capabilities"] = json!(["open-url"]);
        let m = validate(v, "wem-shadertoy").unwrap();

        let on_disk = manifest_json(&m);
        let obj = on_disk.as_object().unwrap();
        // schema 的 properties 名单（与 docs/wem-plugin.schema.json 同步）
        let allowed = [
            "schemaVersion",
            "id",
            "name",
            "summary",
            "description",
            "category",
            "icon",
            "author",
            "version",
            "homepage",
            "minAppVersion",
            "capabilities",
            "entry",
            "url",
            "open",
        ];
        for k in obj.keys() {
            assert!(
                allowed.contains(&k.as_str()),
                "落盘清单出现了协议外的键: {k}"
            );
        }
        for k in ["schemaVersion", "id", "name", "capabilities", "entry"] {
            assert!(obj.contains_key(k), "落盘清单缺少必需键: {k}");
        }
        let entry = obj["entry"].as_object().unwrap();
        for k in entry.keys() {
            assert!(
                ["type", "url", "open"].contains(&k.as_str()),
                "entry 出现了协议外的键: {k}"
            );
        }
        assert_eq!(entry["type"], json!("url"));

        // 回给前端的条目要带上协议字段，界面才能显示能力与兼容性
        let shown = entry_value(&m, None);
        assert_eq!(shown["privileged"], json!(false));
        assert_eq!(shown["schemaVersion"], json!(SCHEMA_VERSION));
        assert_eq!(shown["capabilities"], json!(["open-url"]));
        assert_eq!(shown["minAppVersion"], json!("2.0.0"));
        assert_eq!(shown["compatible"], json!(true));
        assert_eq!(shown["installed"], json!(false));
    }

    /// 协议用例是**宿主与作者脚本共用**的一份：Rust 这边必须全绿，
    /// docs/plugin-template/scripts/validate-wem-plugin.mjs --self-test 也必须全绿。
    /// 两边规则漂移（比如 Rust 放宽了、文档/自查脚本没跟上）会在这里当场红。
    #[test]
    fn shared_protocol_cases_agree() {
        let raw = include_str!("../../../docs/plugin-template/protocol-cases.json");
        let cases: Value = serde_json::from_str(raw).expect("protocol-cases.json 必须是合法 JSON");

        let accept = cases["accept"].as_array().expect("accept 数组");
        assert!(!accept.is_empty());
        for c in accept {
            let hint = c["idHint"].as_str().unwrap_or("wem-case");
            let note = c["note"].as_str().unwrap_or("");
            let m = validate(c["manifest"].clone(), hint)
                .unwrap_or_else(|e| panic!("应当接受（{note}），却被 {} 拒了", e.code));
            if let Some(v) = c["expect"]["kind"].as_str() {
                assert_eq!(m.kind, v, "{note}");
            }
            if let Some(v) = c["expect"]["open"].as_str() {
                assert_eq!(m.open, v, "{note}");
            }
            if let Some(v) = c["expect"]["category"].as_str() {
                assert_eq!(m.category, v, "{note}");
            }
            if let Some(list) = c["expect"]["capabilities"].as_array() {
                let want: Vec<String> = list
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect();
                assert_eq!(m.capabilities, want, "{note}");
            }
            if let Some(list) = c["expect"]["packages"].as_array() {
                let want: Vec<String> = list
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect();
                assert_eq!(m.packages, want, "{note}");
            }
        }

        let reject = cases["reject"].as_array().expect("reject 数组");
        assert!(!reject.is_empty());
        for c in reject {
            let hint = c["idHint"].as_str().unwrap_or("wem-case");
            let note = c["note"].as_str().unwrap_or("");
            let want = c["code"].as_str().unwrap_or("");
            match validate(c["manifest"].clone(), hint) {
                Ok(_) => panic!("应当拒绝（{note}），却通过了"),
                Err(e) => assert_eq!(e.code, want, "错误码不一致（{note}）"),
            }
        }
    }

    #[test]
    fn dsh_entry_round_trips_and_validates_package_specs() {
        let v = json!({
            "name": "DSH 壁纸引擎",
            "capabilities": ["dsh-profile"],
            "entry": { "type": "dsh", "packages": ["dsh-plugin-wallpaper-engine"] }
        });
        let m = validate(v, "wem-dsh-wallpaper").unwrap();
        assert_eq!(m.kind, "dsh");
        assert_eq!(m.capabilities, vec!["dsh-profile".to_string()]);
        assert_eq!(m.packages, vec!["dsh-plugin-wallpaper-engine".to_string()]);
        // dsh 型的窗口恒为应用内窗口，且没有 url
        assert_eq!(m.open, "window");
        assert!(m.url.is_empty());
        // 落盘 → 再读回来必须一致（幂等）
        let on_disk = manifest_json(&m);
        assert_eq!(on_disk["entry"]["type"], json!("dsh"));
        assert!(on_disk["entry"].get("url").is_none());
        let back = validate(on_disk, "wem-dsh-wallpaper").unwrap();
        assert_eq!(back, m);
        // 回给前端的条目带上 kind/packages，界面才知道该走哪条打开路径
        let shown = entry_value(&m, None);
        assert_eq!(shown["kind"], json!("dsh"));
        assert_eq!(shown["packages"], json!(["dsh-plugin-wallpaper-engine"]));
        // 特权标记由宿主算出来（dsh 型必然为真），界面据此弹确认
        assert_eq!(shown["privileged"], json!(true));

        // 包规格白名单
        for ok in [
            "dsh-plugin",
            "@scope/pkg",
            "pkg@1.2.3",
            "pkg@^1.2.0",
            "@scope/pkg@next",
        ] {
            assert!(valid_package_spec(ok), "应当接受: {ok}");
        }
        for bad in [
            "-g",
            "--force",
            "pkg --force",
            "file:../local",
            "link:../local",
            "git+https://github.com/a/b.git",
            "../evil",
            "/abs/path",
            "pkg/../../x",
            "UPPER",
            "_underscore",
            ".hidden",
        ] {
            assert!(!valid_package_spec(bad), "应当拒绝: {bad}");
        }
    }

    /// 平铺形状的清单（入口字段写顶层、不带 `entry` 对象）：宿主要能落成一份**规范
    /// 清单**，并且重新扫描时读得回来、认得特权标记。
    #[test]
    fn flat_entry_installs_as_canonical_dsh_manifest() {
        let root = tmp("flat-dsh");
        let flat = json!({
            "id": "dsh-wallpaper-engine",
            "name": "DSH 壁纸引擎插件",
            "summary": "把本机的 Wallpaper Engine 壁纸装进 DeepSeek Harness 的界面",
            "category": "AI 助手",
            "icon": "🧩",
            "author": "elysia395",
            "kind": "dsh",
            "open": "window",
            "capabilities": ["dsh-profile"],
            "packages": ["dsh-plugin-wallpaper-engine"],
            "homepage": "https://www.npmjs.com/package/dsh-plugin-wallpaper-engine",
            // 展示类字段（source/market/installed）不是清单字段，忽略即可
            "source": "third",
            "market": "local",
            "installed": false
        });
        let entry = write_in(&root, flat, "wem-x").unwrap();
        assert_eq!(entry["kind"], json!("dsh"));
        assert_eq!(entry["installed"], json!(true));
        assert_eq!(entry["privileged"], json!(true));

        let on_disk: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("dsh-wallpaper-engine").join(MANIFEST_FILE))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(on_disk["entry"]["type"], json!("dsh"));
        assert_eq!(
            on_disk["entry"]["packages"],
            json!(["dsh-plugin-wallpaper-engine"])
        );
        assert!(on_disk["entry"].get("url").is_none());

        let items = list_in(&root).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["kind"], json!("dsh"));
        assert_eq!(items[0]["privileged"], json!(true));
        assert_eq!(items[0]["packages"], json!(["dsh-plugin-wallpaper-engine"]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn id_conflict_between_different_sources_is_rejected() {
        let root = tmp("conflict");
        write_in(&root, good(), "wem-shadertoy").unwrap();

        // 同一个 id、同一个来源（主页相同）→ 原地更新
        let mut again = good();
        again["version"] = json!("1.0.1");
        write_in(&root, again, "wem-shadertoy").unwrap();
        assert_eq!(list_in(&root).unwrap().len(), 1);

        // 同一个 id、另一个来源 → 拒绝覆盖
        let mut other = good();
        other["homepage"] = json!("https://github.com/someone-else/wem-shadertoy");
        let err = write_in(&root, other, "wem-shadertoy").unwrap_err();
        assert_eq!(err.code, "plugin-id-conflict");
        // 旧的那份必须原样还在
        let items = list_in(&root).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["version"], json!("1.0.1"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_list_uninstall_round_trip() {
        let root = tmp("roundtrip");
        let entry = write_in(&root, good(), "wem-shadertoy").unwrap();
        assert_eq!(entry["id"], json!("wem-shadertoy"));
        assert_eq!(entry["installed"], json!(true));
        assert_eq!(entry["icon"], json!("✨"));
        assert!(root.join("wem-shadertoy").join(MANIFEST_FILE).is_file());

        let items = list_in(&root).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["name"], json!("Shadertoy"));

        // 重装同一份：原地覆盖，不产生第二份
        write_in(&root, good(), "wem-shadertoy").unwrap();
        assert_eq!(list_in(&root).unwrap().len(), 1);

        remove_in(&root, "wem-shadertoy").unwrap();
        assert!(list_in(&root).unwrap().is_empty());
        // 再卸一次：明确报「没装」，而不是默默成功
        assert_eq!(
            remove_in(&root, "wem-shadertoy").unwrap_err().code,
            "plugin-not-installed"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hand_dropped_dirs_are_hot_and_bad_ones_are_skipped() {
        let root = tmp("hot");
        // 手放一个合法目录（模拟用户直接拷进来）→ 立刻可见，不用重启
        let dir = root.join("manual");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST_FILE), good().to_string()).unwrap();
        // 坏目录：非法 JSON / 没清单 / 非法 url —— 全部跳过，不能让整页报错
        let bad = root.join("bad-json");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join(MANIFEST_FILE), "{ not json").unwrap();
        std::fs::create_dir_all(root.join("empty-dir")).unwrap();
        let evil = root.join("evil");
        std::fs::create_dir_all(&evil).unwrap();
        let mut m = good();
        m["entry"]["url"] = json!("file:///etc/passwd");
        std::fs::write(evil.join(MANIFEST_FILE), m.to_string()).unwrap();

        let items = list_in(&root).unwrap();
        assert_eq!(items.len(), 1, "只有合法的那份应当被列出: {items:?}");
        assert_eq!(items[0]["id"], json!("manual"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn id_hint_survives_raw_github_urls() {
        assert_eq!(
            id_hint_from_url(
                "https://raw.githubusercontent.com/me/wem-shadertoy/HEAD/wem-plugin.json"
            ),
            "wem-shadertoy"
        );
        assert_eq!(
            id_hint_from_url("https://raw.githubusercontent.com/me/wem-x/main/wem-plugin.json"),
            "wem-x"
        );
        assert_eq!(
            id_hint_from_url("https://example.com/plugins/wem-plugin.json"),
            "plugins"
        );
    }

    #[test]
    fn query_and_order_mapping() {
        assert_eq!(
            build_query("wem-plugin", "  shader "),
            "topic:wem-plugin shader"
        );
        assert_eq!(build_query("wem-plugin", ""), "topic:wem-plugin");
        assert_eq!(search_order("stars"), Some(("stars", "desc")));
        assert_eq!(search_order("updated"), Some(("updated", "desc")));
        assert_eq!(search_order("name"), None);
        assert_eq!(search_order("relevance"), None);
    }

    #[test]
    fn parses_github_search_payload() {
        let raw = br#"{
          "total_count": 1,
          "items": [{
            "full_name": "me/wem-shadertoy",
            "name": "wem-shadertoy",
            "description": "shader wallpapers",
            "html_url": "https://github.com/me/wem-shadertoy",
            "stargazers_count": 42,
            "updated_at": "2026-09-01T00:00:00Z",
            "archived": false,
            "topics": ["shader", "wem-plugin"],
            "owner": { "login": "me" }
          }]
        }"#;
        let (total, items) = parse_search(raw).unwrap();
        assert_eq!(total, 1);
        assert_eq!(items[0]["id"], json!("github:me/wem-shadertoy"));
        assert_eq!(items[0]["category"], json!("创作工具"));
        assert_eq!(items[0]["stars"], json!(42));
        assert_eq!(
            items[0]["manifestUrl"],
            json!("https://raw.githubusercontent.com/me/wem-shadertoy/HEAD/wem-plugin.json")
        );
    }

    #[test]
    fn repo_topics_are_matched_as_a_whole_set() {
        // 真实 GitHub 仓库的 topics 按字母序排（第一个往往是 csharp/desktop 这类），
        // 分类必须看整组，不能只看第一个
        let repo = GhRepo {
            full_name: "rocksdanister/lively".into(),
            name: "lively".into(),
            description: Some("Free and open-source wallpaper app".into()),
            html_url: "https://github.com/rocksdanister/lively".into(),
            stargazers_count: 19_696,
            updated_at: Some("2026-09-29T04:54:13Z".into()),
            archived: Some(false),
            topics: Some(vec![
                "csharp".into(),
                "desktop".into(),
                "wallpaper".into(),
                "wallpaper-engine".into(),
            ]),
            owner: GhOwner {
                login: "rocksdanister".into(),
            },
        };
        let entry = market_entry(&repo);
        assert_eq!(entry["category"], json!("壁纸资源"));
        assert_eq!(entry["author"], json!("rocksdanister"));
        assert_eq!(entry["stars"], json!(19_696));
    }
}
