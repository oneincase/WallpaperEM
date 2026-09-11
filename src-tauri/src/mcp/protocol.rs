//! MCP 协议层：JSON-RPC 分发 + tools / resources / prompts。
//!
//! 只实现本服务真正提供的能力：tools（列表/调用）、resources（文档/模板/本地库条目）、
//! prompts（两个创作引导）。不提供 sampling / roots / logging 推送 / 订阅。

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tauri::AppHandle;

use super::{tools, CallLog, McpState, SUPPORTED_PROTOCOLS};

/// 处理一条 JSON-RPC 消息。返回 `None` 表示这是通知（无响应体）。
pub async fn handle_message(app: &AppHandle, st: &McpState, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    if method.is_empty() {
        // 没有 method 的一律是响应/非法消息，MCP 服务端不该收到
        return Some(error(id, -32600, "非法请求：缺少 method"));
    }
    let params = msg.get("params").cloned().unwrap_or(json!({}));

    if method.starts_with("notifications/") {
        return None;
    }

    let result = match method {
        "initialize" => Ok(initialize_result(&params)),
        "ping" => Ok(json!({})),
        "logging/setLevel" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools::definitions() })),
        "tools/call" => return Some(call_tool_message(app, st, id, &params).await),
        "resources/list" => Ok(json!({ "resources": resources_list() })),
        "resources/templates/list" => Ok(json!({ "resourceTemplates": resource_templates() })),
        "resources/read" => read_resource(app, &params),
        "prompts/list" => Ok(json!({ "prompts": prompts_list() })),
        "prompts/get" => get_prompt(&params),
        other => Err((-32601, format!("不支持的方法: {other}"))),
    };

    Some(match result {
        Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }),
        Err((code, message)) => error(id, code, &message),
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize_result(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // 支持就回显客户端版本，否则回最新（规范：服务端应回自己支持的版本）
    let protocol = if SUPPORTED_PROTOCOLS.contains(&requested) {
        requested.to_string()
    } else {
        SUPPORTED_PROTOCOLS[0].to_string()
    };
    json!({
        "protocolVersion": protocol,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "subscribe": false, "listChanged": false },
            "prompts": { "listChanged": false },
            "logging": {},
        },
        "serverInfo": {
            "name": "wallpaperem",
            "title": "WallpaperEM",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": "WallpaperEM 壁纸引擎。推荐流程：projects_list 看现有工程 → project_create 建工程（web 或 scene）\
→ 用 project_write_file 写文件（或直接用你自己的文件工具改 ~/Documents/WallpaperEM/Projects/<工程>/ 下的文件）\
→ project_validate 校验 → scene 类型再 scene_pack 打包 → project_install 装进本地库 → wallpaper_apply 应用到桌面\
→ wallpaper_screenshot 截图看效果，不满意就改文件重来（改完要重新 scene_pack 与 project_install）。\
动手前先读 resources 里的 wallpaperem://docs/web-project 或 wallpaperem://docs/scene-project。"
    })
}

// ---------------------------------------------------------------- tools/call

async fn call_tool_message(
    app: &AppHandle,
    st: &McpState,
    id: Value,
    params: &Value,
) -> Value {
    let Some(name) = params.get("name").and_then(|n| n.as_str()) else {
        return error(id, -32602, "tools/call 缺少 name");
    };
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let started = Instant::now();
    let (content, is_error) = tools::call(app, name, &args).await;
    let ms = started.elapsed().as_millis() as u64;
    let summary = content
        .iter()
        .find(|c| c.get("type").and_then(|t| t.as_str()) == Some("text"))
        .and_then(|c| c.get("text").and_then(|t| t.as_str()))
        .map(|t| t.chars().take(160).collect::<String>())
        .unwrap_or_default();
    st.push_log(CallLog {
        tool: name.to_string(),
        ok: !is_error,
        ms,
        summary,
        // 毫秒：设置页的「最近调用」直接 new Date(at) 渲染时间列
        at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    });
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "content": content, "isError": is_error }
    })
}

// ---------------------------------------------------------------- resources

fn resources_list() -> Vec<Value> {
    vec![
        json!({
            "uri": "wallpaperem://docs/web-project",
            "name": "网页壁纸工程规范",
            "description": "project.json 字段、WE 用户属性、宿主 shim 可用接口与常见坑",
            "mimeType": "text/markdown",
        }),
        json!({
            "uri": "wallpaperem://docs/scene-project",
            "name": "场景壁纸工程规范",
            "description": "scene.json / models / materials / 粒子 / 打包规则与支持范围",
            "mimeType": "text/markdown",
        }),
        json!({
            "uri": "wallpaperem://templates/web-basic",
            "name": "网页壁纸模板",
            "description": "project_create(type=web) 会落盘的四个文件全文",
            "mimeType": "text/plain",
        }),
        json!({
            "uri": "wallpaperem://templates/scene-basic",
            "name": "场景壁纸模板",
            "description": "project_create(type=scene) 会落盘的文件全文（贴图除外）",
            "mimeType": "text/plain",
        }),
    ]
}

fn resource_templates() -> Vec<Value> {
    vec![json!({
        "uriTemplate": "wallpaperem://library/{itemId}",
        "name": "本地库条目",
        "description": "某个已安装壁纸的元数据与用户属性（itemId 形如 custom-xxxxxxxx 或工坊 id）",
        "mimeType": "application/json",
    })]
}

fn read_resource(app: &AppHandle, params: &Value) -> Result<Value, (i64, String)> {
    let uri = params
        .get("uri")
        .and_then(|u| u.as_str())
        .ok_or((-32602, "resources/read 缺少 uri".to_string()))?;
    let text = match uri {
        "wallpaperem://docs/web-project" => {
            include_str!("../../../docs/mcp-authoring-web.md").to_string()
        }
        "wallpaperem://docs/scene-project" => {
            include_str!("../../../docs/mcp-authoring-scene.md").to_string()
        }
        "wallpaperem://templates/web-basic" => template_bundle("web"),
        "wallpaperem://templates/scene-basic" => template_bundle("scene"),
        other => {
            if let Some(item_id) = other.strip_prefix("wallpaperem://library/") {
                let item_id = percent_decode(item_id);
                let detail = tools::library_item_json(app, &item_id)
                    .map_err(|e| (-32602, e))?;
                let props = crate::library::item_props(app.clone(), item_id.clone())
                    .unwrap_or_default();
                serde_json::to_string_pretty(&json!({
                    "item": detail,
                    "properties": serde_json::to_value(props).unwrap_or(json!([])),
                }))
                .map_err(|e| (-32603, e.to_string()))?
            } else {
                return Err((-32602, format!("未知资源: {other}")));
            }
        }
    };
    // 资源在 list 里声明的是 text/markdown（模板则是纯文本），read 要保持同一口径
    let mime = if uri.starts_with("wallpaperem://docs/") {
        "text/markdown"
    } else {
        "text/plain"
    };
    Ok(json!({
        "contents": [{ "uri": uri, "mimeType": mime, "text": text }]
    }))
}

/// 把模板文件拼成一份可读文本（二进制贴图只标注存在与体积）
fn template_bundle(kind: &str) -> String {
    let Some(files) = crate::workspace::template_resource(kind) else {
        return format!("未知模板: {kind}");
    };
    let mut out = format!("# {kind} 模板文件\n\n");
    for (rel, bytes) in files {
        out.push_str(&format!("## {rel}\n\n"));
        match std::str::from_utf8(bytes) {
            Ok(text) => {
                let lang = if rel.ends_with(".json") { "json" } else { "html" };
                out.push_str(&format!("```{lang}\n{text}\n```\n\n"));
            }
            Err(_) => out.push_str(&format!(
                "（二进制文件，{} 字节；project_create 会直接落盘，多数情况下应整张替换）\n\n",
                bytes.len()
            )),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

// ---------------------------------------------------------------- prompts

fn prompts_list() -> Vec<Value> {
    vec![
        json!({
            "name": "create_web_wallpaper",
            "title": "做一个网页壁纸",
            "description": "按规范新建一个 web 壁纸工程并走完安装/应用/截图自检",
            "arguments": [
                { "name": "topic", "description": "画面主题（如「雨夜霓虹城市」）", "required": false },
                { "name": "name", "description": "工程名（留空则按主题起名）", "required": false },
            ],
        }),
        json!({
            "name": "create_scene_wallpaper",
            "title": "做一个场景壁纸",
            "description": "按规范新建一个 scene 壁纸工程、打包 scene.pkg 并走完安装/应用/截图自检",
            "arguments": [
                { "name": "topic", "description": "画面主题（如「雪山极光」）", "required": false },
                { "name": "name", "description": "工程名（留空则按主题起名）", "required": false },
            ],
        }),
    ]
}

fn get_prompt(params: &Value) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or((-32602, "prompts/get 缺少 name".to_string()))?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let topic = args
        .get("topic")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let project = args
        .get("name")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let text = match name {
        "create_web_wallpaper" => format!(
            "用 WallpaperEM 的 MCP 工具做一个网页壁纸{theme}。\n\
             1. 先读资源 wallpaperem://docs/web-project（工程规范与坑）。\n\
             2. project_create(type=\"web\", name=\"{project}\", title=\"...\") 建工程。\n\
             3. 用 project_write_file 重写 index.html / main.js / style.css：画面要有真实的动画（Canvas/WebGL/CSS 都行），\
                不要留模板里的占位内容；需要用户可调参数就写进 project.json 的 general.properties。\n\
             4. project_validate 确认无误 → project_install 安装 → wallpaper_apply 应用 → wallpaper_screenshot 看效果。\n\
             5. 截图不满意就改文件、重新 project_update 并再截图，直到画面符合预期为止。",
            theme = if topic.is_empty() {
                String::new()
            } else {
                format!("，主题：{topic}")
            },
            project = if project.is_empty() { "自拟" } else { &project },
        ),
        "create_scene_wallpaper" => format!(
            "用 WallpaperEM 的 MCP 工具做一个场景壁纸{theme}（WE 原生 scene.json，打包成 scene.pkg）。\n\
             1. 先读资源 wallpaperem://docs/scene-project（支持范围、坐标约定、坑）。\n\
             2. project_create(type=\"scene\", name=\"{project}\", title=\"...\") 建工程。\n\
             3. 用 project_write_file 写 scene.json、models/*.json、materials/*.json；贴图用 base64 写 \
                materials/<名字>.png（scene_pack 会自动转成 .tex 并打进包）。\n\
             4. project_validate → scene_pack → project_install → wallpaper_apply → wallpaper_screenshot。\n\
             5. 截图不满意就改文件（改完必须重新 scene_pack 与 project_install）再看效果。\n\
             注意：v1 支持图片图层 / 视差 / 关键帧动画 / 粒子预设，不支持自定义 shader 效果与 3D 模型。",
            theme = if topic.is_empty() {
                String::new()
            } else {
                format!("，主题：{topic}")
            },
            project = if project.is_empty() { "自拟" } else { &project },
        ),
        other => return Err((-32602, format!("未知 prompt: {other}"))),
    };

    Ok(json!({
        "description": "WallpaperEM 壁纸创作流程",
        "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_echoes_supported_protocol() {
        let r = initialize_result(&json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(r["protocolVersion"], json!("2024-11-05"));
        assert_eq!(r["capabilities"]["tools"]["listChanged"], json!(false));

        let r = initialize_result(&json!({ "protocolVersion": "1999-01-01" }));
        assert_eq!(r["protocolVersion"], json!(SUPPORTED_PROTOCOLS[0]));
    }

    /// 两份创作规范是 `include_str!` 进二进制的 MCP 资源，删了就没法编译；
    /// 这里再挡一道「文件还在但被清空/改了路径」的回归。
    #[test]
    fn authoring_docs_are_bundled() {
        let web = include_str!("../../../docs/mcp-authoring-web.md");
        let scene = include_str!("../../../docs/mcp-authoring-scene.md");
        assert!(web.len() > 2000, "网页规范内容过少: {} 字节", web.len());
        assert!(scene.len() > 2000, "场景规范内容过少: {} 字节", scene.len());
        assert!(web.contains("wallpaperPropertyListener"));
        assert!(scene.contains("scene.json"));
    }

    #[test]
    fn resources_are_addressable() {
        let list = resources_list();
        assert!(list.iter().any(|r| r["uri"] == json!("wallpaperem://docs/scene-project")));
        assert!(template_bundle("web").contains("index.html"));
        assert!(template_bundle("scene").contains("scene.json"));
    }

    #[test]
    fn prompt_requires_known_name() {
        assert!(get_prompt(&json!({"name": "create_web_wallpaper"})).is_ok());
        assert!(get_prompt(&json!({"name": "nope"})).is_err());
    }

    #[test]
    fn percent_decode_handles_escapes() {
        assert_eq!(percent_decode("custom-1%2F2"), "custom-1/2");
        assert_eq!(percent_decode("plain"), "plain");
    }
}
