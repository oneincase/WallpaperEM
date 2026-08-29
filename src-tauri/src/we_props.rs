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
    let text = std::fs::read_to_string(dir.join("project.json")).ok()?;
    serde_json::from_str::<Value>(&text).ok()
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
        .filter(|(_, v)| v.get("type").and_then(|t| t.as_str()).is_some())
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

/// 当前生效的完整属性表（默认值 + 用户覆盖），wire 格式；无 project.json 时为空表
pub fn effective_props(conn: &Connection, wallpapers_dir: &Path, item_id: &str) -> Map<String, Value> {
    let item_dir = wallpapers_dir.join(item_id);
    let Some(project) = load_project(&item_dir) else {
        return Map::new();
    };
    let overrides = read_overrides(conn, item_id);
    let file_prefix = entry_dir_prefix(&item_dir);
    let mut out = Map::new();
    for (name, def) in raw_props(&project) {
        let ptype = def.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if let Some(v) = overrides.get(&name).cloned().or_else(|| wire_value(ptype, &def)) {
            // file 属性：值相对壁纸根存储，下发给壁纸时按入口 HTML 所在目录补相对前缀
            // （WE 语义：文件属性相对路径以入口页面为基准解析）
            let v = if ptype == "file" {
                v.as_str()
                    .map(|s| json!(format!("{file_prefix}{s}")))
                    .unwrap_or(v)
            } else {
                v
            };
            out.insert(name, json!({ "value": v }));
        }
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

/// general.localization 查表：{lang: {key: text}}，语言标签大小写不敏感
fn localized(project: &Value, key: &str) -> Option<String> {
    let table = project
        .get("general")
        .and_then(|g| g.get("localization"))
        .and_then(|l| l.as_object())?;
    for want in TEXT_LANGS {
        let hit = table.iter().find_map(|(lang, entries)| {
            if !lang.eq_ignore_ascii_case(want) {
                return None;
            }
            entries.get(key).and_then(|v| v.as_str())
        });
        if let Some(s) = hit {
            if !s.trim().is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
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
        let decoded: Option<String> = if let Some(hex) =
            body.strip_prefix("#x").or_else(|| body.strip_prefix("#X"))
        {
            u32::from_str_radix(hex, 16)
                .ok()
                .and_then(char::from_u32)
                .map(String::from)
        } else if let Some(dec) = body.strip_prefix('#') {
            dec.parse::<u32>().ok().and_then(char::from_u32).map(String::from)
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
            let ptype = def.get("type").and_then(|t| t.as_str()).unwrap_or("other").to_string();
            let default = wire_value(&ptype, &def);
            let value = overrides.get(&name).cloned().or_else(|| default.clone());
            let text_raw = def.get("text").and_then(|t| t.as_str()).unwrap_or("");
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
                text: resolve_text(&project, text_raw, &name),
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
pub fn set_overrides(conn: &Connection, item_id: &str, values: &Map<String, Value>) -> Result<(), String> {
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
pub fn set_single_override(conn: &Connection, item_id: &str, name: &str, value: Value) -> Result<(), String> {
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
        let text = |n: &str| {
            defs.iter().find(|d| d.name == n).unwrap().text.clone()
        };
        assert_eq!(text("a"), "阿尔法", "zh-chs 命中（语言标签大小写不敏感）");
        assert_eq!(text("b"), "貝塔", "zh-chs 缺该键 → 逐键回退 zh-cht");
        assert_eq!(text("c"), "Gamma", "前两档都缺 → 回退 en-us");
        assert_eq!(text("miss"), "miss", "全表未命中的 ui_ 键 → 回退属性名");
        assert_eq!(text("html"), "定位城市 City", "剥离标签，边界补空格");
        assert_eq!(text("deco"), "deco", "纯 HTML 装饰清理后为空 → 回退属性名");
        assert_eq!(text("ent"), "A & B C", "实体解码");
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
        assert_eq!(strip_html("100% &unknownent; x"), "100% &unknownent; x", "未知实体原样保留");
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
        assert_eq!(props.len(), 3, "条件不成立的属性照样下发");
        assert_eq!(props.get("hidden").unwrap(), &json!({"value": 0.5}));
        assert_eq!(props.get("never").unwrap(), &json!({"value": true}));
    }

    #[test]
    fn effective_props_merges_overrides_and_set_overrides_roundtrip() {
        let conn = mem_db();
        let dir = fixture_dir("merge", "x2", SAMPLE);
        // 默认：screenFile 无值被跳过（6 个属性 − 1），disableRili=false
        let props = effective_props(&conn, &dir, "x2");
        assert_eq!(props.len(), 5);
        assert!(props.get("screenFile").is_none(), "无值的 file 不下发");
        assert_eq!(props.get("disableRili").unwrap(), &json!({"value": false}));

        // 覆盖：改颜色 + 启用开关
        let mut overrides = Map::new();
        overrides.insert("schemecolor".into(), json!("1 0 0"));
        overrides.insert("disableRili".into(), json!(true));
        set_overrides(&conn, "x2", &overrides).unwrap();
        let props = effective_props(&conn, &dir, "x2");
        assert_eq!(props.get("schemecolor").unwrap(), &json!({"value": "1 0 0"}));
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
        let dir = fixture_dir("empty", "nope", "{}");
        assert!(describe(&conn, &dir, "nope").is_empty());
        assert!(effective_props(&conn, &dir, "nope").is_empty());
        // 无 properties 段
        let dir2 = fixture_dir("noprops", "x", r#"{"type":"web","file":"a.html"}"#);
        assert!(describe(&conn, &dir2, "x").is_empty());
    }
}
