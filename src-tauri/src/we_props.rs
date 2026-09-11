//! WE 网页壁纸（type: "web"）的 Wallpaper Engine 兼容层：project.json 用户属性解析。
//!
//! WE 网页壁纸通过 `window.wallpaperPropertyListener` 接收 project.json
//! `general.properties` 中的属性值。本模块负责：
//! - 过滤纯显示项（无 `"type"` 字段的 `tip`/`ui_*` 等不是属性）
//! - 按 WE 线格式转换值（`applyUserProperties(properties)` 收到的 value 类型）：
//!   color → `"r g b"` 空格分隔浮点字符串；bool → 布尔；slider → 数值；
//!   combo → 保留声明的 JSON 类型；text/textinput → 字符串；file → 相对路径字符串
//! - 解析显示文案：`general.localization` 多语言表 → WE 内建键映射 → 剥离 HTML
//! - 合并用户覆盖值（settings 表 `web_props:{item_id}`，JSON 对象 name→wire 值）
//! - 为内容服务器生成 shim 引导 JSON（props + fps），为 UI 生成属性定义列表
//!
//! 显隐条件（`condition`）原样透传给前端求值，本模块不据此过滤 `effective_props`——
//! WE 语义下被隐藏的属性依然要下发给壁纸，过滤会让壁纸读不到值而异常。

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde_json::{json, Map, Value};

/// 用户覆盖值存放的 settings 键前缀
const OVERRIDES_KEY_PREFIX: &str = "web_props:";

/// 全局语言设置键（设置 → 通用 → 语言）。
/// 只影响壁纸内的 `language` 属性，不做软件本体 i18n。
pub const LANGUAGE_SETTING_KEY: &str = "language";
/// WE 全局语言码 → 壁纸 `language` 属性值。
///
/// 注意壁纸作者并不统一用 WE 的语言码：有的用 WE 全码（"english"），有的用
/// 自定义数字（language.value == 3）。全局语言只能注入"WE 语义"的值 ——
/// 壁纸自己声明了 language 属性时一律以壁纸为准（见 effective_props），
/// 只有壁纸没声明时才补这个全局默认，所以数字枚举类壁纸不会被错误覆盖。
pub const LANGUAGE_DEFAULT: &str = "english";
pub const LANGUAGE_CHOICES: [&str; 6] = [
    "simplifiedchinese",
    "traditionalchinese",
    "english",
    "japanese",
    "korean",
    "german",
];

/// 读全局语言，非法/缺失回退英文。
pub fn global_language(conn: &Connection) -> String {
    let raw = crate::db::get_setting(conn, LANGUAGE_SETTING_KEY).unwrap_or_default();
    if LANGUAGE_CHOICES.contains(&raw.as_str()) {
        raw
    } else {
        LANGUAGE_DEFAULT.to_string()
    }
}

/// UI 编辑用的属性定义
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebPropDef {
    pub name: String,
    /// color | bool | slider | combo | text | textinput | file | other
    pub ptype: String,
    /// 显示名（localization 表 → WE 内建映射 → 属性名；已剥离 HTML）
    pub text: String,
    /// 排序键。真实 project.json 里存在 `32.5`、`1151.0022` 这类浮点细分序，
    /// 必须按 f64 读，否则整数化会把同组属性压平成随机序
    pub order: f64,
    /// 当前生效值（覆盖或默认），wire 格式
    pub value: Value,
    /// project.json 默认值，wire 格式；无默认值为 null
    pub default: Value,
    pub overridden: bool,
    /// WE 显隐条件表达式（如 `a.value && b.value == 1`），由前端按当前草稿求值。
    /// 注意：仅用于 UI 显隐，effective_props 不按它过滤（隐藏属性照样要下发给壁纸）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// combo 选项 [{label, value, condition?}]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ComboOption>,
    /// slider 可选范围（project.json 提供时才有，缺省 UI 用 0..1）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// slider 显示精度（小数位数，project.json `precision`；真实数据 51 处）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precision: Option<i64>,
    /// file 属性的期望文件类别（project.json `fileType`：image/video/audio），
    /// 决定选择器过滤器；directory 属性无此字段
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_type: Option<String>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComboOption {
    pub label: String,
    /// 保留 project.json 声明的 JSON 类型（数字/布尔/字符串混用是常态）
    pub value: Value,
    /// 选项级显隐条件（少见但真实存在）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

/// 读取并解析壁纸目录下的 project.json
fn load_project(dir: &Path) -> Option<Value> {
    let mut project = load_project_raw(dir)?;
    merge_preset(dir, &mut project);
    Some(project)
}

/// 不做预设合成的原始读取（供 merge_preset 读依赖用，避免递归）
fn load_project_raw(dir: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(dir.join("project.json")).ok()?;
    serde_json::from_str::<Value>(&text).ok()
}

/// 预设物品（WE "另存为预设"）：project.json 只有扁平的 `preset`（属性名 → 值），
/// 没有 `general.properties` —— 类型/文案/order 全在 `dependency` 指向的基础壁纸里。
/// 不合成的话 raw_props 返回空表，壁纸一个属性都收不到，表现为整页黑屏。
///
/// 合成规则：以基础壁纸的 properties 为骨架（type/order/condition/text 等元信息），
/// 把 preset 的值覆盖进每项的 `value`。preset 独有、基础壁纸没定义的键（本体自加的
/// 属性）按无 type 补一条，走 wire_value 的字符串兜底透传。
///
/// 依赖内容已由下载器 merge_missing 合并进本体目录，所以文件路径基准仍是本体目录，
/// 无需重写 file 属性的值。
fn merge_preset(dir: &Path, project: &mut Value) {
    // 已有 properties 的正常壁纸不动
    if project
        .get("general")
        .and_then(|g| g.get("properties"))
        .and_then(|p| p.as_object())
        .is_some_and(|p| !p.is_empty())
    {
        return;
    }
    let Some(preset) = project.get("preset").and_then(|p| p.as_object()).cloned() else {
        return;
    };
    if preset.is_empty() {
        return;
    }

    // 基础壁纸（dependency 指向）与本体同级，其内容已合并进本体目录
    let base_props = base_wallpaper_props(dir, project);

    let mut merged = Map::new();
    for (name, value) in &preset {
        match base_props.get(name).and_then(|d| d.as_object()) {
            // 有定义：保留元信息，只换 value
            Some(def) => {
                let mut def = def.clone();
                def.insert("value".into(), value.clone());
                merged.insert(name.clone(), Value::Object(def));
            }
            // preset 独有：基础壁纸没定义类型。标成 text 让它通过 raw_props 的
            // 「有 type 才是真属性」过滤（否则会被当成 tip/ui_* 纯显示项丢掉），
            // 值走 wire_value 的字符串兜底，壁纸 JS 自行转型
            None => {
                merged.insert(name.clone(), json!({ "type": "text", "value": value }));
            }
        }
    }

    let general = project
        .as_object_mut()
        .map(|o| o.entry("general").or_insert_with(|| json!({})));
    if let Some(g) = general.and_then(|g| g.as_object_mut()) {
        g.insert("properties".into(), Value::Object(merged));
    }
}

/// 读取 `dependency` 指向的基础壁纸的 general.properties（同一 wallpapers 目录下的兄弟目录）
fn base_wallpaper_props(dir: &Path, project: &Value) -> Map<String, Value> {
    let self_id = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(parent) = dir.parent() else {
        return Map::new();
    };
    let ids = crate::download::parse_dependency_ids(project, &self_id);
    for id in ids {
        let props = load_project_raw(&parent.join(&id))
            .as_ref()
            .and_then(|p| p.get("general"))
            .and_then(|g| g.get("properties"))
            .and_then(|p| p.as_object())
            .cloned();
        if let Some(props) = props.filter(|p| !p.is_empty()) {
            return props;
        }
    }
    Map::new()
}

/// general.properties 原始条目（仅保留有 "type" 的真属性，跳过 tip/ui_* 纯显示项）
fn raw_props(project: &Value) -> Vec<(String, Value)> {
    let Some(props) = project
        .get("general")
        .and_then(|g| g.get("properties"))
        .and_then(|p| p.as_object())
    else {
        return Vec::new();
    };
    let mut out: Vec<(String, Value)> = props
        .iter()
        // 空 type（真实语料里存在 "type": ""）没有可渲染控件，WE 不显示，一并跳过
        .filter(|(_, v)| {
            v.get("type")
                .and_then(|t| t.as_str())
                .is_some_and(|t| !t.trim().is_empty())
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    // order 按 f64 读（浮点细分序常见）；并列时按属性名兜底保证稳定
    out.sort_by(|a, b| {
        let oa = order_of(&a.1);
        let ob = order_of(&b.1);
        oa.partial_cmp(&ob)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

/// 属性排序键；缺失或非数值为 0
fn order_of(def: &Value) -> f64 {
    def.get("order").and_then(|o| o.as_f64()).unwrap_or(0.0)
}

/// 把 project.json 属性默认值转成 WE 线格式；无值（如未选文件的 file）返回 None → 整个属性跳过
fn wire_value(ptype: &str, def: &Value) -> Option<Value> {
    let raw = def.get("value")?;
    Some(match ptype {
        "color" => Value::String(raw.as_str()?.to_string()),
        "bool" => Value::Bool(raw.as_bool()?),
        "slider" => {
            let n = raw.as_f64().or_else(|| raw.as_str()?.parse().ok())?;
            json!(n)
        }
        // combo：保留 project.json 声明的 JSON 类型。选项值数字/布尔/字符串混用是常态，
        // 字符串化会让壁纸里的 === / switch 全部失配
        "combo" => raw.clone(),
        // text/textinput/file 及未知类型按字符串透传（file 为相对壁纸目录的路径）
        _ => Value::String(match raw {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }),
    })
}

/// 读取用户覆盖值（settings 表，JSON 对象 name→wire 值）
fn read_overrides(conn: &Connection, item_id: &str) -> Map<String, Value> {
    crate::db::get_setting(conn, &format!("{OVERRIDES_KEY_PREFIX}{item_id}"))
        .and_then(|s| serde_json::from_str::<Map<String, Value>>(&s).ok())
        .unwrap_or_default()
}

/// 当前生效的完整属性表（默认值 + 用户覆盖），wire 格式；无 project.json 时为空表。
///
/// 全局语言合并（需求：壁纸没有自带语言设置时用全局语言）：
/// 遍历完 project.json 的属性后，若其中**没有**名为 `language` 的属性，
/// 补一条全局语言。壁纸自己声明了 language（无论是 WE 全码还是自定义数字枚举）
/// 都以壁纸为准 —— 否则会把数字枚举类壁纸的 language=3 错误改写成字符串。
pub fn effective_props(
    conn: &Connection,
    wallpapers_dir: &Path,
    item_id: &str,
) -> Map<String, Value> {
    let item_dir = wallpapers_dir.join(item_id);
    let Some(project) = load_project(&item_dir) else {
        return Map::new();
    };
    let overrides = read_overrides(conn, item_id);
    let file_prefix = entry_dir_prefix(&item_dir);
    let mut out = Map::new();
    let mut wallpaper_has_language = false;
    for (name, def) in raw_props(&project) {
        if name == "language" {
            wallpaper_has_language = true;
        }
        let ptype = def.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if let Some(v) = overrides
            .get(&name)
            .cloned()
            .or_else(|| wire_value(ptype, &def))
        {
            // file 属性：值相对壁纸根存储，下发给壁纸时按入口 HTML 所在目录补相对前缀
            // （WE 语义：文件属性相对路径以入口页面为基准解析）；空值不补（空串 + 前缀会凭空指向目录）
            let v = if ptype == "file" {
                v.as_str()
                    .filter(|s| !s.is_empty())
                    .map(|s| json!(format!("{file_prefix}{s}")))
                    .unwrap_or(v)
            } else {
                v
            };
            out.insert(name, json!({ "value": v }));
        }
    }
    if !wallpaper_has_language {
        // 用户对 language 的显式覆盖也走 overrides，但壁纸没声明该属性时 overrides
        // 里也不会有 —— 全局语言是唯一来源。直接补 WE 全码字符串。
        out.entry("language")
            .or_insert_with(|| json!({ "value": global_language(conn) }));
    }
    out
}

/// 入口 HTML 所在子目录前缀（相对壁纸根），如 "web/"、"pages/"；根入口为 ""
fn entry_dir_prefix(dir: &Path) -> String {
    if let Some(project) = load_project(dir) {
        if let Some(f) = project.get("file").and_then(|f| f.as_str()) {
            let rel = f.trim().replace('\\', "/");
            if !rel.is_empty()
                && !rel.starts_with('/')
                && !rel.split('/').any(|seg| seg == "..")
                && dir.join(&rel).is_file()
            {
                return prefix_of(&rel);
            }
        }
    }
    if dir.join("web/index.html").is_file() {
        return "web/".into();
    }
    if dir.join("index.html").is_file() {
        return String::new();
    }
    crate::wallpaper::find_first_html(dir)
        .map(|rel| prefix_of(&rel))
        .unwrap_or_default()
}

fn prefix_of(rel: &str) -> String {
    match rel.rfind('/') {
        Some(i) => rel[..=i].to_string(),
        None => String::new(),
    }
}

/// 内容服务器 HTML 注入用的引导数据 {"props": {...}, "fps": 30|60|120}
pub fn boot_json(db: &Arc<Mutex<Connection>>, wallpapers_dir: &Path, item_id: &str) -> Value {
    let (props, fps) = match db.lock() {
        Ok(conn) => {
            let props = effective_props(&conn, wallpapers_dir, item_id);
            let fps = crate::wallpaper::global_scene_fps(Some(&conn));
            (props, fps)
        }
        Err(_) => (Map::new(), crate::wallpaper::DEFAULT_SCENE_FPS),
    };
    json!({ "props": props, "fps": fps })
}

/// 目录属性（`type: "directory"`）对应的文件清单：属性名 → 该目录内文件的相对 URL 路径。
///
/// WE 的 `wallpaperRequestRandomFileForProperty(prop, cb)` 语义是「从该目录随机取一个文件」。
/// 官方 CEF 直接读文件系统；webwallgl 的 shim 改成从父页预推的清单里挑（`__wePushDirectoryFiles`），
/// 所以这份清单必须由内容服务器在注入时一起下发 —— 不下发的话幻灯片类壁纸拿到空串，
/// 表现为「背景图一直不换」。
///
/// 路径相对**入口 HTML 所在目录**（与 file 属性同一基准），壁纸里可直接当 URL 用。
/// 只扫目录内的普通文件，不递归；越界路径（绝对路径 / `..`）一律跳过。
pub fn directory_files(
    conn: &Connection,
    wallpapers_dir: &Path,
    item_id: &str,
) -> Map<String, Value> {
    let item_dir = wallpapers_dir.join(item_id);
    let Some(project) = load_project(&item_dir) else {
        return Map::new();
    };
    let overrides = read_overrides(conn, item_id);
    let prefix = entry_dir_prefix(&item_dir);
    let mut out = Map::new();

    for (name, def) in raw_props(&project) {
        if def.get("type").and_then(|t| t.as_str()) != Some("directory") {
            continue;
        }
        // 目录值取「用户覆盖 > project.json 默认」，与 effective_props 同一优先级
        let rel = overrides
            .get(&name)
            .and_then(|v| v.as_str())
            .or_else(|| def.get("value").and_then(|v| v.as_str()))
            .unwrap_or("")
            .trim()
            .replace('\\', "/");
        // 空值 / 绝对路径（WE 里表示用户系统目录，包外不可访问）/ 穿越一律跳过
        if rel.is_empty() || rel.starts_with('/') || rel.split('/').any(|s| s == "..") {
            continue;
        }
        let abs = item_dir.join(&rel);
        if !abs.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&abs) else {
            continue;
        };
        let mut files: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .filter_map(|e| {
                let fname = e.file_name().to_string_lossy().into_owned();
                // 隐藏文件（.DS_Store 等）不是壁纸素材
                if fname.starts_with('.') {
                    return None;
                }
                Some(format!("{prefix}{}/{}", rel.trim_end_matches('/'), fname,))
            })
            .collect();
        if files.is_empty() {
            continue;
        }
        // 目录序在不同文件系统上不稳定，排序保证同一壁纸每次拿到的清单一致
        files.sort();
        out.insert(name, json!(files));
    }
    out
}

/// 把用户覆盖值合并进 project.json 响应体，供**场景壁纸**渲染时读取。
///
/// 网页壁纸的属性经 shim 下发（`boot_json` → `applyUserProperties`），但场景壁纸不同：
/// 它的属性作用在 `scene.json` 的字段绑定上（图层可见性/颜色/透明度…），渲染器是从
/// `project.json` 读属性表来解引用这些绑定的，所以覆盖值必须出现在该响应里。
///
/// 覆盖值写进 `general.properties[name].value`，并给被覆盖的属性加 `userOverridden: true`。
/// 这个标记是必需的，不能只改 value：`scene.json` 里每个受属性控制的字段都自带
/// `{user, value}` 快照，而快照与 project.json 默认值并非总是相等（实测 78 个场景的
/// 2598 处引用里有 372 处不等 —— 作者改过属性默认值却没重存场景，或字段名与属性名撞车）。
/// 渲染器据此只在用户**显式改过**该属性时才采用属性表的值，其余沿用场景快照；
/// 否则用户什么都没改，画面就会先变样。
///
/// 解析失败（非法 JSON）或无覆盖值时原样返回，渲染器退回场景快照即既有行为。
pub fn merge_overrides_into_project(conn: &Connection, item_id: &str, raw: Vec<u8>) -> Vec<u8> {
    let overrides = read_overrides(conn, item_id);
    if overrides.is_empty() {
        return raw;
    }
    // project.json 可能带 BOM（真实壁纸里常见），serde 不接受，先剥掉
    let text = String::from_utf8_lossy(&raw);
    let Ok(mut project) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else {
        return raw;
    };
    let Some(props) = project
        .get_mut("general")
        .and_then(|g| g.get_mut("properties"))
        .and_then(|p| p.as_object_mut())
    else {
        return raw;
    };
    for (name, value) in overrides {
        // 属性已被作者移除（壁纸更新过）：陈旧覆盖值忽略，不凭空造出一个属性
        let Some(def) = props.get_mut(&name).and_then(|d| d.as_object_mut()) else {
            continue;
        };
        def.insert("value".into(), value);
        def.insert("userOverridden".into(), Value::Bool(true));
    }
    serde_json::to_vec(&project).unwrap_or(raw)
}

/// 文案解析优先语言，按序逐「键」回退。必须逐键而非逐语言：真实壁纸的
/// localization 表是残缺的（同一壁纸 en-us 有 152 条、其他语言只有 5 条）
const TEXT_LANGS: [&str; 3] = ["zh-chs", "zh-cht", "en-us"];

/// WE 内建 i18n key → 中文显示名。这些键由 WE 客户端自带字符串表提供，
/// 不出现在任何壁纸的 localization 表里，只能硬编码
fn builtin_text(key: &str) -> Option<&'static str> {
    Some(match key {
        "ui_browse_properties_scheme_color" => "主题颜色",
        _ => return None,
    })
}

/// general.localization 查表：{lang: {key: text}}。
/// - 语言标签大小写不敏感（en-us / EN-US 都出现过）
/// - 键查找先精确、再大小写不敏感回退（同一壁纸里键名大小写不一致真实存在）
/// - 首选语言（zh-chs/zh-cht/en-us）逐键回退后仍没有 → 任意语言兜底：
///   显示其它语言的文案总比显示原始属性名/ui_ 键强
fn localized(project: &Value, key: &str) -> Option<String> {
    let table = project
        .get("general")
        .and_then(|g| g.get("localization"))
        .and_then(|l| l.as_object())?;
    fn lookup<'a>(entries: &'a Value, key: &str) -> Option<&'a str> {
        let obj = entries.as_object()?;
        if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
            return Some(s);
        }
        obj.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .and_then(|(_, v)| v.as_str())
    }
    let non_empty = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
    for want in TEXT_LANGS {
        let hit = table.iter().find_map(|(lang, entries)| {
            if !lang.eq_ignore_ascii_case(want) {
                return None;
            }
            lookup(entries, key)
        });
        if let Some(s) = hit.and_then(non_empty) {
            return Some(s);
        }
    }
    table
        .iter()
        .find_map(|(_, entries)| lookup(entries, key).and_then(non_empty))
}

/// 剥离 HTML 标签、解码常见实体、折叠空白。
/// 壁纸文案里 `<br />`、`<h4 class='ugcSuccess'>`、整段打赏 `<a><img></a>` 都很常见
fn strip_html(raw: &str) -> String {
    let mut text = String::with_capacity(raw.len());
    let mut depth = 0usize;
    for ch in raw.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            // 标签内的内容整体丢弃；标签边界补空格，避免 `a<br>b` 粘成 `ab`
            _ if depth > 0 => {}
            _ => text.push(ch),
        }
        if ch == '>' && depth == 0 {
            text.push(' ');
        }
    }
    let text = decode_entities(&text);
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 解码 HTML 实体：数值实体 `&#8470;` / `&#x2030;` 通用处理，命名实体取常见集。
/// 真实壁纸文案里出现过 `&ensp;`（320 处）与 `&#x2030;`（118 处）
fn decode_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        // 在 12 字节窗口内找分号，避免把裸 & 之后的长文本误当实体扫描
        let end = tail
            .char_indices()
            .take_while(|(off, _)| *off < 12)
            .find(|(_, c)| *c == ';')
            .map(|(off, _)| off);
        let Some(end) = end else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let body = &tail[1..end];
        let decoded: Option<String> =
            if let Some(hex) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .map(String::from)
            } else if let Some(dec) = body.strip_prefix('#') {
                dec.parse::<u32>()
                    .ok()
                    .and_then(char::from_u32)
                    .map(String::from)
            } else {
                match body {
                    "lt" => Some("<".into()),
                    "gt" => Some(">".into()),
                    "quot" => Some("\"".into()),
                    "apos" => Some("'".into()),
                    // 各类空格实体统一压成普通空格（随后 split_whitespace 折叠）
                    "nbsp" | "ensp" | "emsp" | "thinsp" => Some(" ".into()),
                    // amp 放在最后一轮解，天然避免 &amp;lt; 被二次解码
                    "amp" => Some("&".into()),
                    _ => None,
                }
            };
        match decoded {
            Some(s) => {
                out.push_str(&s);
                rest = &tail[end + 1..];
            }
            // 未识别的实体原样保留，不吞掉可能有意义的文本
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 属性/选项文案解析：localization 表 → WE 内建映射 → 原文 → 回退值。
/// 对任意 raw 都查表（不限 ui_ 前缀：真实壁纸有 `Dividing_line0` 这类无前缀键）
fn resolve_text(project: &Value, raw: &str, fallback: &str) -> String {
    let resolved = localized(project, raw)
        .or_else(|| builtin_text(raw).map(str::to_string))
        .unwrap_or_else(|| raw.to_string());
    let clean = strip_html(&resolved);
    // 清理后为空（纯 HTML 装饰/打赏横幅）或仍是未解析的 ui_ 键 → 回退
    if clean.is_empty() || clean.starts_with("ui_") {
        return fallback.to_string();
    }
    clean
}

/// 属性定义列表（UI 编辑用）；无 project.json / 无属性时为空
pub fn describe(conn: &Connection, wallpapers_dir: &Path, item_id: &str) -> Vec<WebPropDef> {
    let Some(project) = load_project(&wallpapers_dir.join(item_id)) else {
        return Vec::new();
    };
    let overrides = read_overrides(conn, item_id);
    raw_props(&project)
        .into_iter()
        .map(|(name, def)| {
            // checkbox 是 bool 的别名（真实语料里 2 处）：wire 语义完全相同，
            // 归一化成 bool，避免前端为同一个开关写两套渲染。
            // 类型名统一小写：真实语料里存在 "Text" 这类大写写法
            let raw_type = def.get("type").and_then(|t| t.as_str()).unwrap_or("other");
            let norm = raw_type.to_ascii_lowercase();
            let ptype = if norm == "checkbox" {
                "bool".to_string()
            } else {
                norm
            };
            let default = wire_value(&ptype, &def);
            let value = overrides.get(&name).cloned().or_else(|| default.clone());
            let text_raw = def.get("text").and_then(|t| t.as_str()).unwrap_or("");
            // 分节标题（text/group）与可编辑属性的空 text 回退不同：
            // 可编辑属性回退成属性名（与 WE 一致）；分节标题的空 text 是作者留的
            // 空白间隔（kong10/fengexian3 这类间隔键在真实壁纸里很常见），
            // 回退成属性名会把间隔键名直接显示在面板上，必须保留为空
            let is_header = ptype == "text" || ptype == "group";
            let options = def
                .get("options")
                .and_then(|o| o.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|o| {
                            let label_raw = o.get("label").and_then(|l| l.as_str())?;
                            let value = o.get("value").cloned()?;
                            // 选项标签回退用值本身（选项没有「名字」可回退）
                            let fallback = match &value {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            Some(ComboOption {
                                label: resolve_text(&project, label_raw, &fallback),
                                value,
                                condition: str_field(o, "condition"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            WebPropDef {
                name: name.clone(),
                text: if is_header {
                    resolve_text(&project, text_raw, "")
                } else {
                    resolve_text(&project, text_raw, &name)
                },
                ptype,
                order: order_of(&def),
                overridden: overrides.contains_key(&name),
                value: value.unwrap_or(Value::Null),
                default: default.unwrap_or(Value::Null),
                condition: str_field(&def, "condition"),
                options,
                min: def.get("min").and_then(|m| m.as_f64()),
                max: def.get("max").and_then(|m| m.as_f64()),
                step: def.get("step").and_then(|m| m.as_f64()),
                precision: def.get("precision").and_then(|m| m.as_i64()),
                file_type: str_field(&def, "fileType"),
            }
        })
        .collect()
}

/// 读取非空字符串字段
fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|c| c.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 保存用户覆盖值（UI 直接传 wire 格式值），空对象等价于清除
pub fn set_overrides(
    conn: &Connection,
    item_id: &str,
    values: &Map<String, Value>,
) -> Result<(), String> {
    let key = format!("{OVERRIDES_KEY_PREFIX}{item_id}");
    if values.is_empty() {
        conn.execute("DELETE FROM settings WHERE key = ?1", [key])
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let s = serde_json::to_string(values).map_err(|e| e.to_string())?;
    crate::db::set_setting(conn, &key, &s)
}

/// 合并写入单个属性的覆盖值（file 属性选择文件后调用；值相对壁纸根）
pub fn set_single_override(
    conn: &Connection,
    item_id: &str,
    name: &str,
    value: Value,
) -> Result<(), String> {
    let mut m = read_overrides(conn, item_id);
    m.insert(name.to_string(), value);
    set_overrides(conn, item_id, &m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn
    }

    /// 构造临时壁纸库目录：wallpapers_dir/<item>/project.json（与真实布局一致）
    fn fixture_dir(tag: &str, item: &str, project: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wpem-we-props-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(item)).unwrap();
        std::fs::write(dir.join(item).join("project.json"), project).unwrap();
        dir
    }

    #[test]
    fn global_language_is_injected_when_wallpaper_has_no_language_prop() {
        let conn = mem_db();
        crate::db::set_setting(&conn, LANGUAGE_SETTING_KEY, "japanese").unwrap();
        // 壁纸只有 schemecolor，没有 language
        let dir = fixture_dir(
            "lang-add",
            "100",
            r#"{
            "type":"scene",
            "general":{"properties":{"c":{"type":"color","value":"1 0 0"}}}
        }"#,
        );
        let props = effective_props(&conn, &dir, "100");
        // 补了全局语言
        assert_eq!(
            props
                .get("language")
                .and_then(|v| v.get("value"))
                .and_then(|v| v.as_str()),
            Some("japanese"),
            "壁纸无 language 属性时应补全局语言"
        );
        // 原属性不受影响
        assert!(props.contains_key("c"));
    }

    #[test]
    fn wallpaper_own_language_prop_is_never_overridden() {
        let conn = mem_db();
        crate::db::set_setting(&conn, LANGUAGE_SETTING_KEY, "simplifiedchinese").unwrap();
        // 壁纸自带 language，且是数字枚举（很多多语言壁纸用 1/2/3）
        let dir = fixture_dir(
            "lang-own",
            "101",
            r#"{
            "type":"scene",
            "general":{"properties":{"language":{
                "type":"combo",
                "options":[{"label":"English","value":"1"},{"label":"中文","value":"3"}],
                "value":"1"}}}
        }"#,
        );
        let props = effective_props(&conn, &dir, "101");
        let v = props
            .get("language")
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str());
        assert_eq!(
            v,
            Some("1"),
            "壁纸自带 language（数字枚举）时不得以全局字符串覆盖"
        );
    }

    #[test]
    fn global_language_invalid_value_falls_back_to_default() {
        let conn = mem_db();
        crate::db::set_setting(&conn, LANGUAGE_SETTING_KEY, "klingon").unwrap();
        assert_eq!(global_language(&conn), LANGUAGE_DEFAULT);
    }

    #[test]
    fn checkbox_is_normalized_to_bool() {
        let conn = mem_db();
        let dir = fixture_dir(
            "checkbox",
            "102",
            r#"{
            "type":"scene",
            "general":{"properties":{"on":{"type":"checkbox","value":true}}}
        }"#,
        );
        let defs = describe(&conn, &dir, "102");
        let on = defs
            .iter()
            .find(|d| d.name == "on")
            .expect("checkbox 属性应保留");
        assert_eq!(
            on.ptype, "bool",
            "checkbox 必须归一化成 bool，前端只渲染一套开关"
        );
    }

    /// 既有测试断言的是 project.json 自身属性的处理；全局 language 注入是正交逻辑。
    /// 用它在断言前剔掉注入项，避免给每个计数断言都 +1、又不掩盖真正的属性丢失。
    fn author_props_count(props: &Map<String, Value>) -> usize {
        props.len() - usize::from(props.contains_key("language"))
    }

    const SAMPLE: &str = r#"{
        "file": "bb.html",
        "general": {
            "properties": {
                "tip": { "order": 9, "text": "纯显示项" },
                "schemecolor": { "order": 0, "text": "ui_browse_properties_scheme_color",
                    "type": "color", "value": "0.25 0.83 1" },
                "disableRili": { "order": 4, "text": "禁用日历", "type": "bool", "value": false },
                "screenFile": { "order": 1, "text": "屏幕上的图片", "type": "file" },
                "phoneText": { "order": 3, "type": "textinput", "value": "[{}]" },
                "amount": { "order": 2, "text": "强度", "type": "slider", "value": 0.5 },
                "mode": { "order": 5, "text": "模式", "type": "combo",
                    "value": "auto", "options": [ {"label": "自动", "value": "auto"}, {"label": "手动", "value": "manual"} ] }
            },
            "supportsaudioprocessing": true
        },
        "type": "web"
    }"#;

    #[test]
    fn describe_filters_display_only_and_converts_wire_format() {
        let conn = mem_db();
        let dir = fixture_dir("describe", "x1", SAMPLE);
        let defs = describe(&conn, &dir, "x1");
        // tip（无 type）被过滤；file（无 value）保留定义但值为 null
        assert_eq!(defs.len(), 6, "过滤纯显示项后剩 6 个属性");
        // order 排序：schemecolor(0), screenFile(1), amount(2), phoneText(3), disableRili(4), mode(5)
        assert_eq!(defs[0].name, "schemecolor");
        assert_eq!(defs[0].ptype, "color");
        assert_eq!(defs[0].value, json!("0.25 0.83 1"), "color → 字符串透传");
        assert_eq!(defs[0].text, "主题颜色", "WE i18n key 映射");
        assert_eq!(defs[1].name, "screenFile");
        assert_eq!(defs[1].value, Value::Null, "file 无默认值 → null");
        assert_eq!(defs[2].name, "amount");
        assert_eq!(defs[2].value, json!(0.5), "slider → 数值");
        assert_eq!(defs[3].name, "phoneText");
        assert_eq!(defs[3].text, "phoneText", "无 text 字段 → 回退属性名");
        assert_eq!(defs[4].name, "disableRili");
        assert_eq!(defs[4].value, json!(false), "bool → 布尔");
        assert_eq!(defs[5].name, "mode");
        assert_eq!(defs[5].options.len(), 2, "combo 选项");
    }

    /// 真实数据形态：浮点 order（32.5 / 1151.0022 这类细分序）与重复 order
    #[test]
    fn order_supports_floats_and_ties_break_by_name() {
        let conn = mem_db();
        let dir = fixture_dir(
            "order",
            "x5",
            r#"{"type":"web","general":{"properties":{
                "d": {"order": 33,      "type": "bool", "value": false},
                "b": {"order": 32.5,    "type": "bool", "value": false},
                "a": {"order": 32,      "type": "bool", "value": false},
                "z": {"order": 32,      "type": "bool", "value": false},
                "c": {"order": 1151.0022, "type": "bool", "value": false},
                "neg": {"order": -1,    "type": "bool", "value": false},
                "none": {"type": "bool", "value": false}
            }}}"#,
        );
        let defs = describe(&conn, &dir, "x5");
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        // -1 < 0(缺省) < 32 < 32.5 < 33 < 1151.0022；order 32 并列时 a 在 z 前
        assert_eq!(names, vec!["neg", "none", "a", "z", "b", "d", "c"]);
    }

    /// localization 表逐键回退 + HTML 剥离
    #[test]
    fn text_resolves_via_localization_and_strips_html() {
        let conn = mem_db();
        let dir = fixture_dir(
            "i18n",
            "x6",
            r#"{"type":"web","general":{
                "localization": {
                    "en-us": {"ui_a": "Alpha", "ui_b": "Beta", "ui_c": "Gamma"},
                    "ZH-CHS": {"ui_a": "阿尔法"},
                    "zh-cht": {"ui_b": "貝塔"}
                },
                "properties": {
                    "a":    {"order": 1, "text": "ui_a", "type": "bool", "value": false},
                    "b":    {"order": 2, "text": "ui_b", "type": "bool", "value": false},
                    "c":    {"order": 3, "text": "ui_c", "type": "bool", "value": false},
                    "miss": {"order": 4, "text": "ui_missing", "type": "bool", "value": false},
                    "html": {"order": 5, "text": "<br />定位城市<br />City<br />", "type": "bool", "value": false},
                    "deco": {"order": 6, "text": "<a href='x'><img src='y'></a>", "type": "bool", "value": false},
                    "ent":  {"order": 7, "text": "A &amp; B&nbsp;C", "type": "bool", "value": false}
                }
            }}"#,
        );
        let defs = describe(&conn, &dir, "x6");
        let text = |n: &str| defs.iter().find(|d| d.name == n).unwrap().text.clone();
        assert_eq!(text("a"), "阿尔法", "zh-chs 命中（语言标签大小写不敏感）");
        assert_eq!(text("b"), "貝塔", "zh-chs 缺该键 → 逐键回退 zh-cht");
        assert_eq!(text("c"), "Gamma", "前两档都缺 → 回退 en-us");
        assert_eq!(text("miss"), "miss", "全表未命中的 ui_ 键 → 回退属性名");
        assert_eq!(text("html"), "定位城市 City", "剥离标签，边界补空格");
        assert_eq!(text("deco"), "deco", "纯 HTML 装饰清理后为空 → 回退属性名");
        assert_eq!(text("ent"), "A & B C", "实体解码");
    }

    /// WE 对齐的空/异形态过滤：空 type、大写类型名、空 text 分节标题
    #[test]
    fn empty_type_and_case_and_blank_headers_match_we() {
        let conn = mem_db();
        let dir = fixture_dir(
            "shape",
            "x8",
            r#"{"type":"web","general":{
                "localization": {
                    "ru-ru": {"UI_WIDHT": "Ширина"},
                    "en-us": {"ui_note": "Note"}
                },
                "properties": {
                    "junk":   {"order": 1, "type": "", "value": 1},
                    "header": {"order": 2, "type": "Text", "text": "ui_note"},
                    "spacer": {"order": 3, "type": "text", "text": ""},
                    "width":  {"order": 4, "type": "slider", "text": "ui_widht", "value": 5, "min": 0, "max": 10}
                }
            }}"#,
        );
        let defs = describe(&conn, &dir, "x8");
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(!names.contains(&"junk"), "空 type 不是可配置属性（WE 不显示）");
        let header = defs.iter().find(|d| d.name == "header").unwrap();
        assert_eq!(header.ptype, "text", "类型名大小写归一（Text → text）");
        assert_eq!(header.text, "Note");
        let spacer = defs.iter().find(|d| d.name == "spacer").unwrap();
        assert_eq!(spacer.text, "", "空 text 分节标题不回退成属性名（保留空白间隔）");
        let width = defs.iter().find(|d| d.name == "width").unwrap();
        assert_eq!(
            width.text, "Ширина",
            "首选语言缺失 → 任意语言兜底 + 键大小写不敏感"
        );
    }

    /// 实体解码：真实壁纸用 &ensp; 做选项标签对齐、&#x2030; 表千分号
    #[test]
    fn decodes_named_and_numeric_entities() {
        assert_eq!(strip_html("全部&ensp;/&ensp;All"), "全部 / All");
        assert_eq!(strip_html("速率&#x2030;"), "速率‰");
        assert_eq!(strip_html("&#8470;1"), "№1");
        assert_eq!(strip_html("a&nbsp;&emsp;b"), "a b", "多个空格实体折叠");
        assert_eq!(strip_html("&amp;lt;"), "&lt;", "amp 解出的 & 不被二次解码");
        assert_eq!(strip_html("A &amp; B"), "A & B");
        assert_eq!(
            strip_html("100% &unknownent; x"),
            "100% &unknownent; x",
            "未知实体原样保留"
        );
        assert_eq!(strip_html("Q&A 100&"), "Q&A 100&", "裸 & 不误吞后文");
        assert_eq!(strip_html("&#x2030"), "&#x2030", "缺分号不解码");
    }

    /// combo 选项值保留声明类型（数字/布尔/字符串混用），选项级 condition 透传
    #[test]
    fn combo_preserves_value_types_and_conditions() {
        let conn = mem_db();
        let dir = fixture_dir(
            "combo",
            "x7",
            r#"{"type":"web","general":{"properties":{
                "mode": {"order": 1, "text": "模式", "type": "combo", "value": 1,
                    "condition": "other.value && x.value == 2",
                    "options": [
                        {"label": "普通", "value": 1, "condition": "p.value == 1"},
                        {"label": "纯色", "value": 2},
                        {"label": "关", "value": false},
                        {"label": "自动", "value": "auto"}
                    ]}
            }}}"#,
        );
        let defs = describe(&conn, &dir, "x7");
        let m = &defs[0];
        assert_eq!(m.default, json!(1), "combo 默认值保留数字类型");
        assert_eq!(m.condition.as_deref(), Some("other.value && x.value == 2"));
        assert_eq!(m.options[0].value, json!(1));
        assert_eq!(m.options[0].condition.as_deref(), Some("p.value == 1"));
        assert_eq!(m.options[1].condition, None);
        assert_eq!(m.options[2].value, json!(false), "布尔选项值保型");
        assert_eq!(m.options[3].value, json!("auto"));

        // 下发给壁纸时同样保型（壁纸里的 === / switch 依赖此）
        let props = effective_props(&conn, &dir, "x7");
        assert_eq!(props.get("mode").unwrap(), &json!({"value": 1}));
    }

    /// condition 只用于 UI 显隐：effective_props 不得据此过滤（否则壁纸读不到值）
    #[test]
    fn conditioned_props_are_still_delivered() {
        let conn = mem_db();
        let dir = fixture_dir(
            "cond",
            "x8",
            r#"{"type":"web","general":{"properties":{
                "gate":   {"order": 1, "type": "bool", "value": false},
                "hidden": {"order": 2, "type": "slider", "value": 0.5, "condition": "gate.value"},
                "never":  {"order": 3, "type": "bool", "value": true, "condition": "false"}
            }}}"#,
        );
        let props = effective_props(&conn, &dir, "x8");
        assert_eq!(author_props_count(&props), 3, "条件不成立的属性照样下发");
        assert_eq!(props.get("hidden").unwrap(), &json!({"value": 0.5}));
        assert_eq!(props.get("never").unwrap(), &json!({"value": true}));
    }

    #[test]
    fn merge_overrides_into_project_marks_only_user_edited_props() {
        let conn = mem_db();
        let dir = fixture_dir("mergeproj", "x20", SAMPLE);
        let raw = std::fs::read(dir.join("x20").join("project.json")).unwrap();

        // 无覆盖值：原样返回（渲染器沿用 scene.json 快照，即既有行为）
        assert_eq!(
            merge_overrides_into_project(&conn, "x20", raw.clone()),
            raw,
            "无覆盖值时不得改写响应体"
        );

        set_single_override(&conn, "x20", "schemecolor", json!("1 0 0")).unwrap();
        let out = merge_overrides_into_project(&conn, "x20", raw.clone());
        let v: Value = serde_json::from_slice(&out).unwrap();
        let props = &v["general"]["properties"];
        // 被改过的属性：值替换 + 打标记（渲染器只认带标记的，见 parse.js 的 resolveUserValue）
        assert_eq!(props["schemecolor"]["value"], json!("1 0 0"));
        assert_eq!(props["schemecolor"]["userOverridden"], json!(true));
        // 未改过的属性：值不动，且**不能**有标记，否则渲染器会拿默认值覆盖场景快照
        assert_eq!(props["amount"]["value"], json!(0.5));
        assert!(
            props["amount"].get("userOverridden").is_none(),
            "未被用户改过的属性不得带 userOverridden"
        );
        // 纯显示项（无 type）不受影响
        assert!(props["tip"].get("userOverridden").is_none());
    }

    #[test]
    fn merge_overrides_into_project_tolerates_bom_stale_keys_and_bad_json() {
        let conn = mem_db();
        let dir = fixture_dir("mergeedge", "x21", SAMPLE);
        set_single_override(&conn, "x21", "schemecolor", json!("0 1 0")).unwrap();
        // 已从 project.json 移除的属性（壁纸更新过）：陈旧覆盖值忽略，不得凭空造出属性
        set_single_override(&conn, "x21", "goneProp", json!(1)).unwrap();

        // 带 BOM 的 project.json（真实壁纸里常见）
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(&std::fs::read(dir.join("x21").join("project.json")).unwrap());
        let out = merge_overrides_into_project(&conn, "x21", bom);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            v["general"]["properties"]["schemecolor"]["value"],
            json!("0 1 0")
        );
        assert!(
            v["general"]["properties"].get("goneProp").is_none(),
            "陈旧覆盖值不得凭空生成属性"
        );

        // 非法 JSON：原样返回，让渲染器自己的容错兜底（绝不能返回半截 JSON）
        let bad = b"{ not json".to_vec();
        assert_eq!(merge_overrides_into_project(&conn, "x21", bad.clone()), bad);
    }

    #[test]
    fn effective_props_merges_overrides_and_set_overrides_roundtrip() {
        let conn = mem_db();
        let dir = fixture_dir("merge", "x2", SAMPLE);
        // 默认：screenFile 无值被跳过（6 个属性 − 1），disableRili=false
        let props = effective_props(&conn, &dir, "x2");
        assert_eq!(author_props_count(&props), 5);
        assert!(props.get("screenFile").is_none(), "无值的 file 不下发");
        assert_eq!(props.get("disableRili").unwrap(), &json!({"value": false}));

        // 覆盖：改颜色 + 启用开关
        let mut overrides = Map::new();
        overrides.insert("schemecolor".into(), json!("1 0 0"));
        overrides.insert("disableRili".into(), json!(true));
        set_overrides(&conn, "x2", &overrides).unwrap();
        let props = effective_props(&conn, &dir, "x2");
        assert_eq!(
            props.get("schemecolor").unwrap(),
            &json!({"value": "1 0 0"})
        );
        assert_eq!(props.get("disableRili").unwrap(), &json!({"value": true}));

        // describe 反映覆盖状态与当前值
        let defs = describe(&conn, &dir, "x2");
        let sc = defs.iter().find(|d| d.name == "schemecolor").unwrap();
        assert!(sc.overridden);
        assert_eq!(sc.value, json!("1 0 0"));
        assert_eq!(sc.default, json!("0.25 0.83 1"));

        // 清除覆盖 → 回默认
        set_overrides(&conn, "x2", &Map::new()).unwrap();
        assert!(
            crate::db::get_setting(&conn, "web_props:x2").is_none(),
            "空覆盖 = 删除 settings 键"
        );
    }

    #[test]
    fn boot_json_contains_props_and_fps() {
        let conn = mem_db();
        let dir = fixture_dir("boot", "x3", SAMPLE);
        let db = Arc::new(Mutex::new(conn));
        let boot = boot_json(&db, &dir, "x3");
        assert_eq!(boot["fps"], crate::wallpaper::DEFAULT_SCENE_FPS);
        assert_eq!(boot["props"]["schemecolor"]["value"], json!("0.25 0.83 1"));
        assert!(boot["props"].get("tip").is_none());
    }

    #[test]
    fn file_prop_values_get_entry_dir_prefix() {
        let conn = mem_db();
        // 入口在 web/ 子目录：file 属性值应补 "web/" 前缀
        let dir = fixture_dir("prefix", "x4", SAMPLE);
        std::fs::create_dir_all(dir.join("x4/web")).unwrap();
        std::fs::write(dir.join("x4/web/index.html"), "<html></html>").unwrap();
        std::fs::remove_file(dir.join("x4/project.json")).unwrap();
        std::fs::write(
            dir.join("x4/project.json"),
            r#"{"type":"web","general":{"properties":{"screenFile":{"type":"file","value":"old.png"}}}}"#,
        )
        .unwrap();
        let props = effective_props(&conn, &dir, "x4");
        assert_eq!(
            props.get("screenFile").unwrap(),
            &json!({"value": "web/old.png"}),
            "file 值按入口目录补前缀"
        );

        // 用户覆盖值同样补前缀（覆盖存的是相对壁纸根的路径）
        let mut overrides = Map::new();
        overrides.insert("screenFile".into(), json!("we-props/screenFile_user.png"));
        set_overrides(&conn, "x4", &overrides).unwrap();
        let props = effective_props(&conn, &dir, "x4");
        assert_eq!(
            props.get("screenFile").unwrap(),
            &json!({"value": "web/we-props/screenFile_user.png"})
        );
    }

    #[test]
    fn missing_project_json_yields_empty() {
        let conn = mem_db();
        // project.json 存在但为空对象：作者属性为空（describe 不含全局注入的 language）
        let dir = fixture_dir("empty", "nope", "{}");
        assert!(describe(&conn, &dir, "nope").is_empty());
        // effective_props 仍会补全局语言（这是张有效壁纸，只是作者没写属性）。
        // 作者属性数量（剔掉注入的 language）必须为 0
        let eff = effective_props(&conn, &dir, "nope");
        assert_eq!(author_props_count(&eff), 0);
        assert_eq!(
            eff.get("language")
                .and_then(|v| v.get("value"))
                .and_then(|v| v.as_str()),
            Some(LANGUAGE_DEFAULT)
        );
        // 无 properties 段
        let dir2 = fixture_dir("noprops", "x", r#"{"type":"web","file":"a.html"}"#);
        assert!(describe(&conn, &dir2, "x").is_empty());
        assert_eq!(author_props_count(&effective_props(&conn, &dir2, "x")), 0);
        // 真正没有 project.json 文件：整体为空（连 language 都无从判定壁纸类型）
        let dir3 = fixture_dir("nofile", "y", "{}");
        std::fs::remove_file(dir3.join("y").join("project.json")).unwrap();
        assert!(effective_props(&conn, &dir3, "y").is_empty());
    }

    /// 真实数据形态：fileType/precision 透传；directory 不补入口前缀；file 空值不补前缀
    #[test]
    fn file_type_precision_passthrough_and_prefix_edge_cases() {
        let conn = mem_db();
        let dir = fixture_dir(
            "extra",
            "x9",
            r#"{"type":"web","file":"web/index.html","general":{"properties":{
                "music": {"order":1, "type":"file", "fileType":"video", "value":""},
                "slide": {"order":2, "type":"directory", "value":""},
                "gain":  {"order":3, "type":"slider", "value":1, "min":0, "max":2, "step":0.1, "precision":1}
            }}}"#,
        );
        let defs = describe(&conn, &dir, "x9");
        let music = defs.iter().find(|d| d.name == "music").unwrap();
        assert_eq!(music.file_type.as_deref(), Some("video"), "fileType 透传");
        let gain = defs.iter().find(|d| d.name == "gain").unwrap();
        assert_eq!(gain.precision, Some(1), "precision 透传");

        let props = effective_props(&conn, &dir, "x9");
        assert_eq!(
            props.get("music").unwrap(),
            &json!({ "value": "" }),
            "file 空默认值不加前缀"
        );
        assert_eq!(
            props.get("slide").unwrap(),
            &json!({ "value": "" }),
            "directory 不加前缀"
        );

        // directory 覆盖值为绝对路径（WE 语义）：同样不加前缀
        we_props_set_dir(&conn, "x9", "slide", "/Users/me/Pictures/album");
        let props = effective_props(&conn, &dir, "x9");
        assert_eq!(
            props.get("slide").unwrap(),
            &json!({ "value": "/Users/me/Pictures/album" })
        );
    }

    /// 预设物品（WE「另存为预设」）：本体只有扁平 preset，类型定义在 dependency 指向的
    /// 基础壁纸里。合成失败会让壁纸一个属性都收不到 —— 真实表现是整页黑屏。
    #[test]
    fn preset_item_inherits_property_types_from_dependency() {
        let conn = mem_db();
        let dir = std::env::temp_dir().join("wpem-we-props-test-preset");
        let _ = std::fs::remove_dir_all(&dir);
        // 基础壁纸：提供 type/order/condition 等元信息
        std::fs::create_dir_all(dir.join("2000")).unwrap();
        std::fs::write(
            dir.join("2000").join("project.json"),
            r#"{"type":"web","file":"index.html","general":{"properties":{
                "bgvideo": { "type": "file", "fileType": "video", "order": 10,
                             "text": "背景视频", "value": "" },
                "vol":     { "type": "slider", "order": 20, "min": 0, "max": 100, "value": 50 },
                "enabled": { "type": "bool", "order": 30, "value": false },
                "tip":     { "order": 40, "text": "纯显示项" }
            }}}"#,
        )
        .unwrap();
        // 预设物品：只有 preset，值覆盖基础壁纸默认值；末两项基础壁纸未定义
        std::fs::create_dir_all(dir.join("1000")).unwrap();
        std::fs::write(
            dir.join("1000").join("project.json"),
            r#"{"dependency":"2000","preset":{
                "bgvideo": "files/clip.webm",
                "vol": 80,
                "enabled": true,
                "custom_flag": "yes"
            }}"#,
        )
        .unwrap();
        // 入口 HTML（依赖内容已由下载器 merge_missing 合并进本体）
        std::fs::write(dir.join("1000").join("index.html"), "<html></html>").unwrap();

        let props = effective_props(&conn, &dir, "1000");
        assert_eq!(
            author_props_count(&props),
            4,
            "preset 的四个键都要下发：{props:?}"
        );
        // 类型来自基础壁纸：slider 是数值不是字符串，bool 是布尔
        assert_eq!(props.get("vol").unwrap(), &json!({ "value": 80.0 }));
        assert_eq!(props.get("enabled").unwrap(), &json!({ "value": true }));
        // file 走入口前缀逻辑（根入口 → 无前缀），值取 preset 覆盖
        assert_eq!(
            props.get("bgvideo").unwrap(),
            &json!({ "value": "files/clip.webm" })
        );
        // 基础壁纸未定义的 preset 键按字符串透传，不能丢
        assert_eq!(
            props.get("custom_flag").unwrap(),
            &json!({ "value": "yes" })
        );

        // describe 也要能列出（属性编辑弹窗依赖它），且带上基础壁纸的元信息
        let defs = describe(&conn, &dir, "1000");
        let vol = defs.iter().find(|d| d.name == "vol").expect("vol 在列表里");
        assert_eq!(vol.ptype, "slider");
        assert_eq!(vol.max, Some(100.0));
        // 纯显示项 tip 不是属性，不进列表
        assert!(defs.iter().all(|d| d.name != "tip"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 预设物品的基础壁纸缺失（依赖没下载）时不能 panic，preset 值仍按字符串下发 ——
    /// 有值总比黑屏强
    #[test]
    fn preset_without_dependency_still_delivers_values() {
        let conn = mem_db();
        let dir = fixture_dir(
            "preset-nodep",
            "1000",
            r#"{"dependency":"9999","preset":{"a":"x","b":3}}"#,
        );
        let props = effective_props(&conn, &dir, "1000");
        assert_eq!(author_props_count(&props), 2);
        assert_eq!(props.get("a").unwrap(), &json!({ "value": "x" }));
        // 无类型定义 → 字符串兜底（壁纸 JS 自行转型）
        assert_eq!(props.get("b").unwrap(), &json!({ "value": "3" }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 复用 set_single_override（测试里经pub接口写覆盖）
    fn we_props_set_dir(conn: &Connection, item: &str, name: &str, path: &str) {
        crate::we_props::set_single_override(conn, item, name, json!(path)).unwrap();
    }

    /// 目录属性清单：路径相对入口目录、排序稳定、隐藏文件与越界路径被拒。
    ///
    /// 这条链路断了是**静默失效** —— 壁纸照常显示，只是幻灯片永远不换图，
    /// 所以必须有测试盯着。
    #[test]
    fn directory_files_lists_relative_to_entry_dir() {
        const PROJ: &str = r#"{
            "file": "web/index.html",
            "general": { "properties": {
                "album":   { "order": 1, "type": "directory", "value": "media/pics" },
                "outside": { "order": 2, "type": "directory", "value": "/Users/me/Pictures" },
                "escape":  { "order": 3, "type": "directory", "value": "../../etc" },
                "missing": { "order": 4, "type": "directory", "value": "nope" },
                "notdir":  { "order": 5, "type": "text",      "value": "media/pics" }
            } }
        }"#;
        let conn = mem_db();
        let root = fixture_dir("dirfiles", "d1", PROJ);
        let item = root.join("d1");
        std::fs::create_dir_all(item.join("web")).unwrap();
        std::fs::write(item.join("web/index.html"), "<html></html>").unwrap();
        let pics = item.join("media/pics");
        std::fs::create_dir_all(&pics).unwrap();
        // 乱序写入，验证输出是排过序的
        std::fs::write(pics.join("c.png"), b"x").unwrap();
        std::fs::write(pics.join("a.jpg"), b"x").unwrap();
        std::fs::write(pics.join("b.webp"), b"x").unwrap();
        std::fs::write(pics.join(".DS_Store"), b"x").unwrap();
        std::fs::create_dir_all(pics.join("nested")).unwrap();

        let files = directory_files(&conn, &root, "d1");

        // 只有可访问的包内目录进清单
        assert_eq!(
            files.keys().collect::<Vec<_>>(),
            vec!["album"],
            "绝对路径/穿越/不存在的目录/非 directory 类型都不该出现"
        );
        // 路径相对入口目录（web/），排序稳定，隐藏文件与子目录被排除
        assert_eq!(
            files.get("album").unwrap(),
            &json!([
                "web/media/pics/a.jpg",
                "web/media/pics/b.webp",
                "web/media/pics/c.png"
            ])
        );

        // 用户改了目录 → 清单跟着走
        std::fs::create_dir_all(item.join("alt")).unwrap();
        std::fs::write(item.join("alt/z.png"), b"x").unwrap();
        we_props_set_dir(&conn, "d1", "album", "alt");
        let files = directory_files(&conn, &root, "d1");
        assert_eq!(files.get("album").unwrap(), &json!(["web/alt/z.png"]));

        // 空目录不下发（推空数组会让 shim 以为有清单却挑不出东西）
        std::fs::create_dir_all(item.join("empty")).unwrap();
        we_props_set_dir(&conn, "d1", "album", "empty");
        assert!(directory_files(&conn, &root, "d1").is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }
}
