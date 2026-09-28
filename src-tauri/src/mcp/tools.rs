//! MCP 工具实现：每个工具都是既有能力（workspace / library / wallpaper / workshop）
//! 的一层薄封装 —— 这里不重复实现任何业务逻辑，只做参数解析与结果整形。

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::{json, Map, Value};
use tauri::{AppHandle, Manager};

use crate::library::LibraryFilter;
use crate::workspace;

/// 部分工具的**结构化输出** schema（MCP 2025-06-18 的 `outputSchema`）。
///
/// 只给形状稳定、agent 最常消费的几个声明：声明了就必须在结果里给
/// `structuredContent` 且能对上（见 `protocol::call_tool_message`）。schema 刻意写得
/// **宽松**（不写 required、`additionalProperties: true`）—— 目的是让客户端能做类型
/// 分发/展示，不是当校验器用；写太严只会让未来的字段增补变成破坏性变更。
fn output_schema_for(name: &str) -> Option<Value> {
    let obj = |props: Value| json!({ "type": "object", "properties": props, "additionalProperties": true });
    let arr_str = || json!({ "type": "array", "items": { "type": "string" } });
    Some(match name {
        "projects_list" => obj(json!({
            "root": { "type": "string" },
            "projects": { "type": "array", "items": obj(json!({
                "project": { "type": "string" },
                "path": { "type": "string" },
                "type": { "type": "string" },
                "title": { "type": "string" },
                "packed": { "type": "boolean" },
                "installedItemId": { "type": ["string", "null"] },
            })) },
        })),
        "project_validate" => obj(json!({
            "ok": { "type": "boolean" },
            "project": { "type": "string" },
            "type": { "type": "string" },
            "title": { "type": "string" },
            "entry": { "type": ["string", "null"] },
            "errors": arr_str(),
            "warnings": arr_str(),
        })),
        "scene_inspect" => obj(json!({
            "ok": { "type": "boolean" },
            "project": { "type": "string" },
            "layerCount": { "type": "integer" },
            "layers": { "type": "array", "items": obj(json!({
                "name": { "type": "string" },
                "kind": { "type": "string" },
                "textures": arr_str(),
                "bindings": arr_str(),
            })) },
            "textures": { "type": "array", "items": obj(json!({
                "name": { "type": "string" },
                "bytes": { "type": "integer" },
                "usedBy": arr_str(),
                "builtin": { "type": "boolean" },
            })) },
            "unusedAssets": arr_str(),
            "properties": obj(json!({
                "declared": arr_str(),
                "bound": { "type": "object" },
                "unbound": arr_str(),
            })),
            "errors": arr_str(),
            "warnings": arr_str(),
        })),
        "scene_pack" => obj(json!({
            "project": { "type": "string" },
            "bytes": { "type": "integer" },
            "convertedTextures": arr_str(),
            "entries": { "type": "array", "items": obj(json!({
                "name": { "type": "string" },
                "bytes": { "type": "integer" },
            })) },
            "warnings": arr_str(),
        })),
        "project_install" => obj(json!({
            "project": { "type": "string" },
            "itemId": { "type": "string" },
            "title": { "type": "string" },
            "type": { "type": "string" },
        })),
        "project_update" => obj(json!({
            "project": { "type": "string" },
            "itemId": { "type": "string" },
            "mode": { "type": "string", "enum": ["incremental", "install"] },
            "copied": { "type": "integer" },
            "removed": { "type": "integer" },
            "unchanged": { "type": "integer" },
        })),
        "renderer_diag" => obj(json!({
            "entries": { "type": "array", "items": obj(json!({
                "at": { "type": "integer" },
                "label": { "type": "string" },
                "item": { "type": "string" },
                "msg": { "type": "string" },
                "failed": { "type": "boolean" },
            })) },
            "shown": { "type": "integer" },
            "total": { "type": "integer" },
            "buffered": { "type": "integer" },
            "cap": { "type": "integer" },
            "last": { "type": "string" },
            "failReason": { "type": "string" },
            "readyItem": { "type": "string" },
        })),
        "particle_recipe" => obj(json!({
            "kind": { "type": "string" },
            "texture": { "type": "string" },
            "files": { "type": "array", "items": { "type": "object" } },
        })),
        "effect_scaffold" => obj(json!({
            "kind": { "type": "string" },
            "files": { "type": "array", "items": { "type": "object" } },
        })),
        "wallpaper_preview" => obj(json!({
            "itemId": { "type": "string" },
            "label": { "type": "string" },
            "frames": { "type": "array", "items": obj(json!({
                "tMs": { "type": "integer" },
                "bytes": { "type": "integer" },
                "mimeType": { "type": "string" },
            })) },
        })),
        "wallpaper_screenshot" => obj(json!({
            "itemId": { "type": "string" },
            "bytes": { "type": "integer" },
            "applied": { "type": "boolean" },
        })),
        _ => return None,
    })
}

/// 把工具结果里的 JSON 文本块抽成 `structuredContent`（MCP 2025-06-18）。
///
/// 只在结果是对象/数组时给：标量（"ok"、数字）没有结构可言，硬塞反而让客户端为难。
/// 文本块**照旧保留** —— 老客户端读 text、新客户端读 structuredContent，两边都不破。
pub fn structured_content(content: &[Value]) -> Option<Value> {
    let text = content
        .iter()
        .find(|c| c.get("type").and_then(|t| t.as_str()) == Some("text"))?
        .get("text")?
        .as_str()?;
    let v: Value = serde_json::from_str(text).ok()?;
    matches!(v, Value::Object(_) | Value::Array(_)).then_some(v)
}

/// 工具清单（name / description / inputSchema + 部分工具的 outputSchema）
pub fn definitions() -> Vec<Value> {
    let obj = |props: Value, required: Value| json!({ "type": "object", "properties": props, "required": required });
    let mut defs = vec![
        json!({
            "name": "projects_list",
            "description": "列出工作区（~/Documents/WallpaperEM/Projects）里的全部壁纸工程及其类型、是否已打包、是否已安装。",
            "inputSchema": obj(json!({}), json!([])),
        }),
        json!({
            "name": "project_create",
            "description": "按模板新建工程：web（HTML/JS 网页壁纸）或 scene（WE 原生 scene.json 场景壁纸）。已存在同名工程时需 force=true 覆盖。",
            "inputSchema": obj(json!({
                "name": { "type": "string", "description": "工程名（同时是目录名与后续工具的 project 参数）" },
                "type": { "type": "string", "enum": ["web", "scene"], "description": "模板类型，默认 web" },
                "title": { "type": "string", "description": "壁纸标题（本地库展示用），默认与工程名相同" },
                "force": { "type": "boolean", "description": "同名工程已存在时是否覆盖" },
            }), json!(["name"])),
        }),
        json!({
            "name": "project_write_file",
            "description": "写工程内文件（自动建目录）。贴图等二进制内容用 encoding=base64（单文件上限 40 MB，base64 会膨胀 4/3，所以原图建议 < 30 MB；更大就写进工程目录再打包）。写入会让旧的 scene.pkg 失效并被删除。写 materials/<名字>.png 时会顺手删掉同名的 .tex，避免打包器优先用旧 .tex。",
            "inputSchema": obj(json!({
                "project": { "type": "string" },
                "path": { "type": "string", "description": "工程内相对路径，如 scene.json 或 materials/bg.png" },
                "content": { "type": "string" },
                "encoding": { "type": "string", "enum": ["utf8", "base64"], "description": "默认 utf8" },
            }), json!(["project", "path", "content"])),
        }),
        json!({
            "name": "project_read_file",
            "description": "读工程内文件（文本按 utf8 返回，二进制按 base64 返回）。",
            "inputSchema": obj(json!({
                "project": { "type": "string" },
                "path": { "type": "string" },
                "maxBytes": { "type": "integer", "description": "读取上限，默认 1 MiB" },
            }), json!(["project", "path"])),
        }),
        json!({
            "name": "project_list_files",
            "description": "列出工程内全部文件及体积（不含隐藏文件与打包产物）。",
            "inputSchema": obj(json!({ "project": { "type": "string" } }), json!(["project"])),
        }),
        json!({
            "name": "project_validate",
            "description": "静态校验工程：project.json 结构与用户属性、网页入口、scene.json 对象与贴图引用链。返回 errors/warnings。",
            "inputSchema": obj(json!({ "project": { "type": "string" } }), json!(["project"])),
        }),
        json!({
            "name": "scene_pack",
            "description": "把 scene 工程打包成 scene.pkg（materials 下的 PNG/JPEG 自动转成 .tex 并内嵌）。打包前会先校验，有 error 会拒绝打包。install=true 时紧接着装/更新进本地库（改完一步到位，省一次往返）。",
            "inputSchema": obj(json!({
                "project": { "type": "string" },
                "install": { "type": "boolean", "description": "打包后直接装/更新进本地库（已装过则增量更新，item_id 不变），默认 false" },
            }), json!(["project"])),
        }),
        json!({
            "name": "scene_inspect",
            "description": "场景工程结构化体检（比 project_validate 更宽）：图层树与类型、每张贴图的尺寸/体积/引用者、未使用素材、属性绑定命中表、粒子组件支持情况、关键帧摘要，外加一份性能提示。校验结果（errors/warnings）一并返回。",
            "inputSchema": obj(json!({ "project": { "type": "string" } }), json!(["project"])),
        }),
        json!({
            "name": "project_import_asset",
            "description": "把宿主上的一个大素材直接拷进工程（不占 JSON-RPC 请求体，单文件上限 512 MB）—— 超过 project_write_file 的 40 MB base64 上限时用它。来源只允许：图片/下载/桌面/文稿/音乐/影片/临时目录与壁纸工程根，扩展名限 png/jpg/jpeg/tex/frag/vert/h/json/ogg/wav/mp3/gif/mp4。",
            "inputSchema": obj(json!({
                "project": { "type": "string" },
                "path": { "type": "string", "description": "工程内目标相对路径，如 materials/bg.png" },
                "source": { "type": "string", "description": "宿主上的绝对路径（必须位于允许的目录内）" },
                "overwrite": { "type": "boolean", "description": "目标已存在时是否覆盖，默认 false" },
            }), json!(["project", "path", "source"])),
        }),
        json!({
            "name": "renderer_diag",
            "description": "读渲染器诊断历史（最近 300 条）：挂载过程、贴图/视频加载失败、效果 pass 被跳过（[we-scene] 告警）、ready/failed 时刻。截图或画面异常时先看它——「效果静默消失」「贴图没找到」这类问题只在这里可见。",
            "inputSchema": obj(json!({
                "label": { "type": "string", "description": "只看某个壁纸窗口 label（list_sessions 里的键）" },
                "itemId": { "type": "string", "description": "只看某个本地库条目的诊断" },
                "sinceMs": { "type": "integer", "description": "只要这个时间戳（epoch 毫秒）之后的" },
                "limit": { "type": "integer", "description": "最多返回多少条（默认 50，上限 300）" },
                "clear": { "type": "boolean", "description": "读之前先清空（下一轮迭代从干净历史开始），默认 false" },
            }), json!([])),
        }),
        json!({
            "name": "project_delete",
            "description": "删除工作区里的工程目录（不影响已安装到本地库的副本）。",
            "inputSchema": obj(json!({ "project": { "type": "string" } }), json!(["project"])),
        }),
        json!({
            "name": "project_install",
            "description": "把工程安装进本地库（拷贝 + 登记），返回 itemId；之后可用 wallpaper_apply / item_props_set。",
            "inputSchema": obj(json!({ "project": { "type": "string" } }), json!(["project"])),
        }),
        json!({
            "name": "project_update",
            "description": "工程改完后覆盖安装：重新拷贝并保持 itemId 不变（用户改过的属性覆盖不丢）。",
            "inputSchema": obj(json!({
                "project": { "type": "string" },
                "itemId": { "type": "string", "description": "可选：显式指定要覆盖的本地库条目 id" },
            }), json!(["project"])),
        }),
        json!({
            "name": "wallpaper_apply",
            "description": "把本地库里的壁纸应用到桌面。缺省应用到所有显示器；传 displayId（见 displays_list）只应用到指定屏。手动应用单张 = 退出轮播：先暂停轮播、再移除覆盖这些屏的轮播上下文（列表实体保留，可再 playlist_apply 启用）。",
            "inputSchema": obj(json!({
                "itemId": { "type": "string" },
                "displayId": { "type": "string", "description": "可选：只应用到该显示器 id（displays_list 返回）" },
            }), json!(["itemId"])),
        }),
        json!({
            "name": "wallpaper_stop",
            "description": "停止壁纸（可选只停某块屏）。",
            "inputSchema": obj(json!({ "displayId": { "type": "string" } }), json!([])),
        }),
        json!({
            "name": "wallpaper_pause",
            "description": "暂停所有壁纸的渲染。",
            "inputSchema": obj(json!({}), json!([])),
        }),
        json!({
            "name": "wallpaper_resume",
            "description": "恢复所有壁纸的渲染。",
            "inputSchema": obj(json!({}), json!([])),
        }),
        json!({
            "name": "wallpaper_screenshot",
            "description": "对当前壁纸窗口抓一帧并作为图片内容返回。优先让渲染器自己抓帧（scene/gif/image/video 三平台都可），抓不到时回退平台原生快照（仅 macOS，web 类型只有它能拍）。默认先应用到桌面再看效果；同一张壁纸已经挂在屏上、且库里的文件没比屏幕上这份更新时会跳过重复应用（不会把大 scene.pkg 再解析一遍），连拍很快。改完工程重新 project_update 之后，这里会自动重新挂载（比的是内容时间，不是 itemId），拿到的一定是新版画面；要强制重挂传 force=true。",
            "inputSchema": obj(json!({
                "itemId": { "type": "string", "description": "要截图的本地库条目 id" },
                "displayId": { "type": "string", "description": "可选：只拍这块屏（displays_list 返回的 id）。多屏各挂不同壁纸时用它点名；给了它就不走「已挂在屏上」快路径，且 apply 只作用于该屏" },
                "apply": { "type": "boolean", "description": "是否先应用该壁纸，默认 true；该壁纸已就绪时自动跳过重复应用" },
                "force": { "type": "boolean", "description": "强制重新应用（即使该壁纸已就绪），默认 false；改完工程想确保截到新版时用" },
                "settleMs": { "type": "integer", "description": "渲染就绪后额外等待毫秒数，默认 1500" },
                "maxWidth": { "type": "integer", "description": "抓帧最大宽度（等比缩放），默认 1920；截图只用于看效果/当封面，压小可显著减小回传体积" },
                "framesMs": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "多帧截图：相对首帧就绪的毫秒时刻数组（如 [0, 1000, 3000]，最多 8 个）。用于验收动画/循环/关键帧——单张图看不出时间维度的问题。每帧一个 image 内容块，顺序与请求一致。注意：窗口被完全遮挡时系统会降频渲染，几帧可能一模一样（此时把应用切到前台再看）",
                },
                "timeoutMs": { "type": "integer", "description": "等待渲染就绪的上限，默认 60000（大 scene.pkg 冷启动可能要 30s+）" },
                "saveAsPreview": { "type": "boolean", "description": "是否把截图写成工程的 preview.png，默认 true" },
                "project": { "type": "string", "description": "可选：工程名（存 preview 用；不传则按 itemId 反查）" },
            }), json!(["itemId"])),
        }),
        json!({
            "name": "wallpaper_preview",
            "description": "**离屏预览**：把某张壁纸（或工程）在一扇用户看不见的窗口里渲染并抓帧返回，**完全不碰桌面壁纸会话** —— 迭代时不必把它挂到桌面上。scene/gif/image/video 都可；web 类型需要平台原生快照（仅 macOS）。比 wallpaper_screenshot 更适合「改一处看一眼」的循环。",
            "inputSchema": obj(json!({
                "itemId": { "type": "string", "description": "本地库条目 id；给 project 时可以省略" },
                "project": { "type": "string", "description": "工程名：会用它在本地库里的副本（没装过就先装一次），方便「改完直接看」" },
                "width": { "type": "integer", "description": "预览窗逻辑宽，默认 1280" },
                "height": { "type": "integer", "description": "预览窗逻辑高，默认 720" },
                "maxWidth": { "type": "integer", "description": "抓帧最大宽度（等比缩放），默认等于 width" },
                "framesMs": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "多帧：相对首帧就绪的毫秒时刻数组（如 [0,1000,3000]，最多 8 个）",
                },
                "timeoutMs": { "type": "integer", "description": "等这扇预览窗首帧就绪的上限，默认 60000" },
                "keepOpen": { "type": "boolean", "description": "保留离屏画布不释放（调试用），默认 false；保留的实例在 180s 内没有新的抓帧请求时会自动释放" },
                "openMainWindow": { "type": "boolean", "description": "主窗口被关闭/回收时是否允许宿主重新打开它当作渲染面（会弹到前台）。默认 false：直接报错并提示替代方案" },
            }), json!([])),
        }),
        json!({
            "name": "list_sessions",
            "description": "当前各屏的壁纸会话（label → 配置）与全局暂停态。",
            "inputSchema": obj(json!({}), json!([])),
        }),
        json!({
            "name": "displays_list",
            "description": "显示器列表（id/名称/位置/缩放/主屏）与每屏当前壁纸会话摘要，含统一/独立模式。id 即 wallpaper_apply / wallpaper_stop 的 displayId。",
            "inputSchema": obj(json!({}), json!([])),
        }),
        json!({
            "name": "active_items",
            "description": "当前已应用的本地库条目 id 列表。",
            "inputSchema": obj(json!({}), json!([])),
        }),
        json!({
            "name": "playlist_list",
            "description": "切换列表（轮播播放列表）列表。",
            "inputSchema": obj(json!({}), json!([])),
        }),
        json!({
            "name": "playlist_create",
            "description": "新建切换列表。intervalSec 最小 30；shuffle=随机播放（洗牌队列，一轮内不重复、可回退）。",
            "inputSchema": obj(json!({
                "name": { "type": "string" },
                "itemIds": { "type": "array", "items": { "type": "string" }, "description": "本地库条目 id，数组顺序即播放顺序" },
                "intervalSec": { "type": "integer", "description": "切换间隔（秒），默认 600，最小 30" },
                "shuffle": { "type": "boolean", "description": "随机播放，默认 false" },
            }), json!(["name", "itemIds"])),
        }),
        json!({
            "name": "playlist_update",
            "description": "更新切换列表（缺省字段不改）。条目或随机开关变化会重建轮播队列。",
            "inputSchema": obj(json!({
                "id": { "type": "integer" },
                "name": { "type": "string" },
                "itemIds": { "type": "array", "items": { "type": "string" } },
                "intervalSec": { "type": "integer" },
                "shuffle": { "type": "boolean" },
            }), json!(["id"])),
        }),
        json!({
            "name": "playlist_delete",
            "description": "删除切换列表（删除当前激活列表时一并停止轮播）。",
            "inputSchema": obj(json!({ "id": { "type": "integer" } }), json!(["id"])),
        }),
        json!({
            "name": "playlist_apply",
            "description": "激活切换列表：立即应用第一项并开始轮播（自动剪掉文件已丢失的条目）。",
            "inputSchema": obj(json!({ "id": { "type": "integer" } }), json!(["id"])),
        }),
        json!({
            "name": "playlist_status",
            "description": "轮播状态：当前列表/进度/间隔/是否暂停与下次切换时间（nextAtMs）；switchable = 有没有可切换的轮播上下文（暂停不影响，上一张/下一张只要 switchable 为真就可用）。",
            "inputSchema": obj(json!({}), json!([])),
        }),
        json!({
            "name": "playlist_stop",
            "description": "停止轮播（清除激活的切换列表；壁纸停在当前这张不动）。",
            "inputSchema": obj(json!({}), json!([])),
        }),
        json!({
            "name": "wallpaper_next",
            "description": "切换列表：下一张（手动切换会重置轮播计时；暂停轮播也照旧可用）。displayId 缺省按当前模式走（统一=全局；独立=各绑定屏各自前进），传值只切该屏。没有可切换的轮播上下文时报错。",
            "inputSchema": obj(json!({
                "displayId": { "type": "string", "description": "可选：只切该显示器的轮播" },
            }), json!([])),
        }),
        json!({
            "name": "wallpaper_prev",
            "description": "切换列表：上一张（随机模式沿洗牌队列回退；暂停轮播也照旧可用）。displayId 语义同 wallpaper_next。",
            "inputSchema": obj(json!({
                "displayId": { "type": "string", "description": "可选：只切该显示器的轮播" },
            }), json!([])),
        }),
        json!({
            "name": "display_binding_set",
            "description": "绑定/解绑某显示器的轮播列表（独立模式的每屏上下文）。绑定后该屏立即开始轮播该列表；playlistId 不传=解绑（回到固定单张，壁纸保持不动）。",
            "inputSchema": obj(json!({
                "displayId": { "type": "string" },
                "playlistId": { "type": "integer", "description": "要轮播的切换列表 id；不传=解绑" },
            }), json!(["displayId"])),
        }),
        json!({
            "name": "wallpaper_rotation_set",
            "description": "暂停/恢复轮播的定时自动切换（不影响壁纸渲染本身）。",
            "inputSchema": obj(json!({ "paused": { "type": "boolean" } }), json!(["paused"])),
        }),
        json!({
            "name": "layer_selfcheck",
            "description": "**判定某层/某效果到底画出来没有**：同一场景渲染两次（原样 vs 把该层 visible 置 false），按像素差分给出结论 —— drawn / flat_block（纯色块，通常就是「效果被静默跳过、退回内置材质」）/ invisible（没有任何贡献，通常就是「shader 没跑」）。比自己盯着截图猜靠谱，失败原因见 wallpaperem://reference/pitfalls。",
            "inputSchema": obj(json!({
                "project": { "type": "string", "description": "工程名" },
                "layer": { "type": "string", "description": "图层名（name 字段）或图层 id（数字）" },
                "settleMs": { "type": "integer", "description": "每次抓帧前的稳定等待，默认 1200" },
                "timeoutMs": { "type": "integer", "description": "等渲染就绪上限，默认 60000" },
                "maxWidth": { "type": "integer", "description": "抓帧宽度，默认 960（判定用途，不需要大图）" },
                "threshold": { "type": "integer", "description": "像素判定为「有差异」的阈值，默认 8（0~255）" },
            }), json!(["project", "layer"])),
        }),
        json!({
            "name": "pitfall_search",
            "description": "按关键词查踩坑清单（症状→原因→改法）。比通读 wallpaperem://reference/pitfalls 省 token；关键词支持中文/英文/文件名，如「.vert」「白块」「粒子」「angles」「硬边」。",
            "inputSchema": obj(json!({
                "query": { "type": "string", "description": "关键词（空则返回全部 id 列表与摘要）" },
                "limit": { "type": "integer", "description": "最多返回条数，默认 5" },
            }), json!([])),
        }),
        json!({
            "name": "particle_recipe",
            "description": "**先拿配方再写文件**：返回一套可直接用 project_write_file 落盘的粒子预设 + 材质（贴图走渲染库内置程序化贴图，工程里不用带位图）。比对着渲染库源码猜字段快得多，也避开已废弃/未实现的写法。",
            "inputSchema": obj(json!({
                "kind": {
                    "type": "string",
                    "enum": ["star_dust", "embers", "nebula_wisps", "meteors"],
                    "description": "star_dust 星尘漂浮 / embers 上飘余烬 / nebula_wisps 巨大絮状 / meteors 偶发流星",
                },
                "name": { "type": "string", "description": "预设名（默认与 kind 同名）；决定 particles/presets/<名>.json 与 materials/presets/<名>.json 两个路径" },
                "maxCount": { "type": "integer", "description": "覆盖粒子数上限（默认按配方：尘埃 170 / 余烬 64 / 絮 26 / 流星 8）" },
                "rate": { "type": "number", "description": "覆盖发射率（每秒）；省略则用配方默认（尘埃/絮 = 维持池满，余烬 26，流星 0.8）" },
            }), json!(["kind"])),
        }),
        json!({
            "name": "effect_scaffold",
            "description": "**先拿脚手架再写文件**：返回一个能直接跑通的自写效果着色器所需**全部文件**（effects/<名>.json + materials/effects/<名>.json + shaders/effects/<名>.frag + .vert）与图层片段。四类现成 shader：水面波纹 / 指针光晕 / 音频条 / 七段时钟。",
            "inputSchema": obj(json!({
                "kind": {
                    "type": "string",
                    "enum": ["water_ripple", "pointer_aura", "audio_bars", "clock", "post_bloom", "film_grain"],
                    "description": "shader 类型（内容与 wallpaperem://reference/effects 的 recipes 一致）。post_bloom/film_grain 是**全屏后期**：图层要用 models/util/fullscreenlayer.json，g_Texture0 = 下面已画好的画面，强度绑图层 alpha",
                },
                "name": { "type": "string", "description": "效果名，决定 effects/<名>.json 与 shaders/effects/<名>.frag（默认与 kind 同名）" },
                "layer": { "type": "string", "description": "可选：把图层声明片段也返回（写 scene.json 的对象里粘贴）" },
            }), json!(["kind"])),
        }),
        json!({
            "name": "library_list",
            "description": "列出本地库壁纸（已安装/已下载），支持类型、标题关键字、标签与排序。tagGroups 里可用两个库内专用值：`$project` = 本软件自己的工程（project_* 建出来又装进库的），`$local` = 本地导入（custom-*）。",
            "inputSchema": obj(json!({
                "type": { "type": "string", "description": "video / scene / web / gif，留空不过滤" },
                "query": { "type": "string", "description": "标题模糊搜索" },
                "tagGroups": {
                    "type": "array",
                    "items": { "type": "array", "items": { "type": "string" } },
                    "description": "分组标签：组内并集、组间交集。Steam 标签名照工坊写（Scene/Video/Anime…）；两个库内专用值：$project（自建工程）、$local（本地导入）",
                },
                "sort": { "type": "string", "description": "downloaded_desc（默认）/ downloaded_asc / title_asc / title_desc / size_desc / size_asc" },
                "limit": { "type": "integer", "description": "默认 50，上限 500" },
                "offset": { "type": "integer", "description": "默认 0" },
            }), json!([])),
        }),
        json!({
            "name": "library_delete",
            "description": "从本地库删除壁纸（含磁盘文件与数据库记录，不可撤销）。只想移出库、保留文件用 library_remove。",
            "inputSchema": obj(json!({ "itemId": { "type": "string" } }), json!(["itemId"])),
        }),
        json!({
            "name": "library_remove",
            "description": "把壁纸移出本地库但保留磁盘文件（清库记录/自定义属性/列表归属，不可撤销）。",
            "inputSchema": obj(json!({ "itemId": { "type": "string" } }), json!(["itemId"])),
        }),
        json!({
            "name": "library_open_folder",
            "description": "在文件管理器里打开某个本地库壁纸的目录。",
            "inputSchema": obj(json!({ "itemId": { "type": "string" } }), json!(["itemId"])),
        }),
        json!({
            "name": "item_props_get",
            "description": "读某个壁纸的用户属性定义与当前值（project.json 默认 + 用户覆盖）。",
            "inputSchema": obj(json!({ "itemId": { "type": "string" } }), json!(["itemId"])),
        }),
        json!({
            "name": "item_props_set",
            "description": "设置壁纸用户属性（保存后对已应用的窗口热更新）。reset=true 时清除全部覆盖值。",
            "inputSchema": obj(json!({
                "itemId": { "type": "string" },
                "values": { "type": "object", "description": "属性名 → 值（color 为 \"r g b\" 字符串，slider 数字，checkbox 布尔）" },
                "reset": { "type": "boolean" },
            }), json!(["itemId"])),
        }),
        json!({
            "name": "workshop_search",
            "description": "搜索 Steam 创意工坊壁纸（需要已登录下载账号；首次会走网络）。",
            "inputSchema": obj(json!({
                "query": { "type": "string" },
                "type": { "type": "string", "description": "video / scene / web / gif / application" },
                "sort": { "type": "string", "description": "trend（默认）/ totaluniquesubscribers / totalfavorited / timecreated" },
                "tags": { "type": "array", "items": { "type": "string" } },
                "page": { "type": "integer" },
            }), json!([])),
        }),
        json!({
            "name": "workshop_item",
            "description": "取某个工坊条目的详情（标题/描述/标签/预览图）。",
            "inputSchema": obj(json!({ "id": { "type": "string" } }), json!(["id"])),
        }),
        #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
        json!({
            "name": "workshop_upload",
            "description": "把工程上传到 Steam 创意工坊（需要 Steam 客户端运行且账号拥有 Wallpaper Engine）。已有 workshop.fileId 的工程=更新原条目，否则新建。需要 preview 图（preview.png/jpg/gif）。注意版权/授权：上传他人制作的壁纸前必须确认已获得对方许可，并遵守 Steam 订阅者协议与工坊规则。",
            "inputSchema": obj(json!({
                "project": { "type": "string" },
                "title": { "type": "string", "description": "可选：工坊标题，缺省用 project.json 的 title" },
                "description": { "type": "string", "description": "工坊描述（建议写清楚用法与来源）" },
                "tags": { "type": "array", "items": { "type": "string" }, "description": "可选：标签（含年龄分级 Everyone/Questionable/Mature，必须与 Steam 工坊标签逐字符一致），缺省用 project.json 的 tags" },
                "visibility": { "type": "string", "description": "public（默认）/ friends / private" },
                "changelog": { "type": "string", "description": "可选：更新说明（更新已有条目时显示）" },
            }), json!(["project"])),
        }),
        #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
        json!({
            "name": "workshop_upload_status",
            "description": "查询工坊上传任务进度/结果（job_id 缺省 = 全部任务）。status=done 时含 workshopUrl 与 publishedfileid。",
            "inputSchema": obj(json!({
                "jobId": { "type": "string", "description": "workshop_upload 返回的任务 id" },
            }), json!([])),
        }),
        json!({
            "name": "download_enqueue",
            "description": "把工坊条目加入下载队列（需要已配置下载账号）。",
            "inputSchema": obj(json!({ "itemId": { "type": "string" } }), json!(["itemId"])),
        }),
        json!({
            "name": "download_list",
            "description": "下载队列最近 100 条任务与状态。",
            "inputSchema": obj(json!({}), json!([])),
        }),
        json!({
            "name": "download_cancel",
            "description": "取消某个下载任务。",
            "inputSchema": obj(json!({ "id": { "type": "integer" } }), json!(["id"])),
        }),
        json!({
            "name": "download_retry",
            "description": "重试某个失败的下载任务。",
            "inputSchema": obj(json!({ "id": { "type": "integer" } }), json!(["id"])),
        }),
    ];
    // 声明了 outputSchema 的工具，结果里必须有对应的 structuredContent（见 protocol）
    for d in defs.iter_mut() {
        let name = d
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(schema) = output_schema_for(&name) {
            if let Some(o) = d.as_object_mut() {
                o.insert("outputSchema".into(), schema);
            }
        }
    }
    defs
}

// ---------------------------------------------------------------- 调用入口

// ---------------------------------------------------------------- 调用入口

/// 一个工具调用需要持有的**串行化键**（0~2 个）。
///
/// 分两个命名空间，因为「工程目录」与「库内条目」是两份数据：
///
/// - `project:<名>`：工程内的读写/校验/体检/打包
/// - `item:<id>`：库内条目的读（预览、截图）、写（删/移）、以及**安装/更新时的写入**
///
/// 安装/更新同时碰两边（读工程、写库），所以两把都要拿 —— 只锁工程的话，
/// 「`scene_pack(install=true)` 正在增量同步库目录」与「`wallpaper_preview` 正在读同一
/// 个库条目」会撞上：预览可能读到写了一半的 `scene.pkg`/贴图，表现为偶发黑屏或旧画面。
///
/// 多把键按**字典序**获取（见 `call`），因此不会出现 A 等 B、B 等 A 的死锁。
fn lock_keys(name: &str, args: &Value) -> Vec<String> {
    const PROJECT_TOOLS: [&str; 9] = [
        "project_write_file",
        "project_read_file",
        "project_list_files",
        "project_validate",
        "scene_inspect",
        "scene_pack",
        "project_install",
        "project_update",
        "project_import_asset",
    ];
    // 会写库目录的工具：安装/更新（工程 → 库），以及删/移（库自身）
    const INSTALL_TOOLS: [&str; 2] = ["project_install", "project_update"];
    const ITEM_TOOLS: [&str; 4] = [
        "wallpaper_preview",
        "wallpaper_screenshot",
        "library_delete",
        "library_remove",
    ];
    let mut keys: Vec<String> = Vec::new();
    let project = args
        .get("project")
        .and_then(|p| p.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if PROJECT_TOOLS.contains(&name) {
        if let Some(p) = project {
            keys.push(format!("project:{p}"));
        }
    }
    // 条目 id：显式 itemId 优先；安装/更新时用「工程名 → 条目 id」的确定性映射
    let mut item = args
        .get("itemId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    if item.is_none() && INSTALL_TOOLS.contains(&name) {
        if let Some(p) = project {
            item = Some(crate::library::project_item_id_for(p));
        }
    }
    if ITEM_TOOLS.contains(&name) || INSTALL_TOOLS.contains(&name) || name == "scene_pack" {
        // 没给 itemId 时按工程名推条目 id（工程名 → 条目 id 是确定性映射）。
        // ⚠️ 预览/安装这类工具**允许只传 project**（"改完直接看"那条路），漏掉这一步
        // 就等于「工程名调用完全没锁」—— 预览正读库目录、安装正在写同一份，正是要防的竞态。
        if item.is_none() {
            if let Some(p) = project {
                item = Some(crate::library::project_item_id_for(p));
            }
        }
        if let Some(id) = item {
            keys.push(format!("item:{id}"));
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

/// 取（必要时创建）某个键的互斥锁
fn resource_lock(key: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut m = map.lock().unwrap_or_else(|e| e.into_inner());
    m.entry(key.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

pub async fn call(app: &AppHandle, name: &str, args: &Value) -> (Vec<Value>, bool) {
    // 同一资源串行（不同资源并行）：见 lock_keys 的说明。
    // 多把键按字典序获取 —— 全局一致的顺序 = 不会死锁。
    let mut guards = Vec::new();
    for key in lock_keys(name, args) {
        guards.push(resource_lock(&key).lock_owned().await);
    }
    let _guards = guards;
    match call_inner(app, name, args).await {
        Ok(mut v) => {
            // 截图结果里夹着 base64 图（`_image`）：抽成 MCP 的 image 内容块，
            // 文本块只留元数据，避免把几 MB 的 base64 再打印一遍
            if let Some((data, mime)) = extract_image(&v) {
                if let Some(obj) = v.as_object_mut() {
                    obj.remove("_image");
                }
                return (
                    vec![
                        json!({ "type": "image", "data": data, "mimeType": mime }),
                        text_content(&v).remove(0),
                    ],
                    false,
                );
            }
            // 多帧截图（`framesMs`）：一帧一个 image 块，顺序与请求一致，元数据在文本块里
            let frames = extract_images(&v);
            if !frames.is_empty() {
                if let Some(obj) = v.as_object_mut() {
                    obj.remove("_images");
                }
                let mut blocks: Vec<Value> = frames
                    .into_iter()
                    .map(|(data, mime)| json!({ "type": "image", "data": data, "mimeType": mime }))
                    .collect();
                blocks.extend(text_content(&v));
                return (blocks, false);
            }
            (text_content(&v), false)
        }
        Err(e) => (text_content(&json!({ "error": e })), true),
    }
}

async fn call_inner(app: &AppHandle, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "projects_list" => {
            let app = app.clone();
            blocking(move || workspace::list_projects(&app)).await
        }
        "project_create" => {
            let app = app.clone();
            let name_ = req_str(args, "name")?;
            let kind = opt_str(args, "type").unwrap_or_else(|| "web".into());
            let title = opt_str(args, "title");
            let force = opt_bool(args, "force");
            blocking(move || {
                workspace::create_project(&app, &name_, &kind, title.as_deref(), force)
            })
            .await
        }
        "project_write_file" => {
            let app = app.clone();
            let project = req_str(args, "project")?;
            let path = req_str(args, "path")?;
            let content = req_str(args, "content")?;
            let encoding = opt_str(args, "encoding").unwrap_or_else(|| "utf8".into());
            blocking(move || {
                workspace::write_project_file(&app, &project, &path, &content, &encoding)
            })
            .await
        }
        "project_read_file" => {
            let app = app.clone();
            let project = req_str(args, "project")?;
            let path = req_str(args, "path")?;
            let max = args
                .get("maxBytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(1 << 20) as usize;
            blocking(move || workspace::read_project_file(&app, &project, &path, max)).await
        }
        "project_list_files" => {
            let app = app.clone();
            let project = req_str(args, "project")?;
            blocking(move || workspace::list_project_files(&app, &project)).await
        }
        "project_validate" => {
            let app = app.clone();
            let project = req_str(args, "project")?;
            blocking(move || workspace::validate_project(&app, &project)).await
        }
        "scene_inspect" => {
            let app = app.clone();
            let project = req_str(args, "project")?;
            blocking(move || crate::scene_inspect::inspect(&app, &project)).await
        }
        "project_import_asset" => {
            let app = app.clone();
            let project = req_str(args, "project")?;
            let path = req_str(args, "path")?;
            let source = req_str(args, "source")?;
            let overwrite = opt_bool(args, "overwrite");
            blocking(move || workspace::import_asset(&app, &project, &path, &source, overwrite))
                .await
        }
        "renderer_diag" => {
            let q = crate::content_server::DiagQuery {
                label: opt_str(args, "label"),
                item: opt_str(args, "itemId"),
                since_ms: args.get("sinceMs").and_then(|v| v.as_u64()).unwrap_or(0),
                limit: args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize,
                clear: opt_bool(args, "clear"),
            };
            Ok(crate::content_server::diag_snapshot(app, &q))
        }
        "scene_pack" => {
            let app = app.clone();
            let project = req_str(args, "project")?;
            let install = opt_bool(args, "install");
            blocking(move || workspace::pack_scene(&app, &project, install)).await
        }
        "project_delete" => {
            let app = app.clone();
            let project = req_str(args, "project")?;
            blocking(move || workspace::delete_project(&app, &project)).await
        }
        "project_install" => {
            let app = app.clone();
            let project = req_str(args, "project")?;
            let name2 = project.clone();
            let (item_id, title, wtype) =
                blocking(move || workspace::install_project(&app, &name2)).await?;
            Ok(json!({
                "project": project,
                "itemId": item_id,
                "title": title,
                "type": wtype,
                "hint": "下一步可 wallpaper_apply 应用，或 wallpaper_screenshot 看效果",
            }))
        }
        "project_update" => {
            let app = app.clone();
            let project = req_str(args, "project")?;
            let item_id = opt_str(args, "itemId");
            blocking(move || workspace::update_project(&app, &project, item_id.as_deref())).await
        }
        "wallpaper_apply" => {
            let app = app.clone();
            let item_id = req_str(args, "itemId")?;
            let display = opt_str(args, "displayId");
            let id2 = item_id.clone();
            blocking(move || crate::wallpaper::apply_item(app, id2, display)).await?;
            Ok(json!({ "applied": item_id }))
        }
        "wallpaper_stop" => {
            let app = app.clone();
            let display = opt_str(args, "displayId");
            blocking(move || crate::wallpaper::stop(app, display)).await?;
            Ok(json!({ "stopped": true }))
        }
        "wallpaper_pause" => {
            let app = app.clone();
            blocking(move || crate::wallpaper::pause_all(app)).await?;
            Ok(json!({ "paused": true }))
        }
        "wallpaper_resume" => {
            let app = app.clone();
            blocking(move || crate::wallpaper::resume_all(app)).await?;
            Ok(json!({ "paused": false }))
        }
        "layer_selfcheck" => layer_selfcheck(app, args).await,
        "pitfall_search" => pitfall_search(args),
        "particle_recipe" => particle_recipe(args),
        "effect_scaffold" => effect_scaffold(args),
        "wallpaper_screenshot" => screenshot(app, args).await,
        "wallpaper_preview" => preview(app, args).await,
        "list_sessions" => list_sessions(app),
        "displays_list" => crate::wallpaper::displays_list(app.clone()),
        "playlist_list" => {
            let app = app.clone();
            let v = blocking(move || crate::wallpaper::playlist_list(app)).await?;
            Ok(json!({ "playlists": v }))
        }
        "playlist_create" => {
            let app = app.clone();
            let name = req_str(args, "name")?;
            let ids: Vec<String> = args
                .get("itemIds")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let interval = args
                .get("intervalSec")
                .and_then(|v| v.as_i64())
                .unwrap_or(600);
            let shuffle = args.get("shuffle").and_then(|v| v.as_bool());
            let id = blocking(move || {
                crate::wallpaper::playlist_create(app, name, ids, interval, shuffle)
            })
            .await?;
            Ok(json!({ "id": id }))
        }
        "playlist_update" => {
            let app = app.clone();
            let id = args.get("id").and_then(|v| v.as_i64()).ok_or("缺少 id")?;
            let name = opt_str(args, "name");
            let ids: Option<Vec<String>> =
                args.get("itemIds").and_then(|v| v.as_array()).map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                });
            let interval = args.get("intervalSec").and_then(|v| v.as_i64());
            let shuffle = args.get("shuffle").and_then(|v| v.as_bool());
            let p = blocking(move || {
                crate::wallpaper::playlist_update(app, id, name, ids, interval, shuffle)
            })
            .await?;
            Ok(json!(p))
        }
        "playlist_delete" => {
            let app = app.clone();
            let id = args.get("id").and_then(|v| v.as_i64()).ok_or("缺少 id")?;
            blocking(move || crate::wallpaper::playlist_delete(app, id)).await?;
            Ok(json!({ "deleted": true }))
        }
        "playlist_apply" => {
            let app = app.clone();
            let id = args.get("id").and_then(|v| v.as_i64()).ok_or("缺少 id")?;
            blocking(move || crate::wallpaper::playlist_apply(app, id)).await
        }
        "playlist_status" => {
            let app = app.clone();
            blocking(move || crate::wallpaper::playlist_status(app)).await
        }
        "playlist_stop" => {
            let app = app.clone();
            blocking(move || crate::wallpaper::playlist_stop(app)).await?;
            Ok(json!({ "stopped": true }))
        }
        "wallpaper_next" => {
            let app = app.clone();
            let display = opt_str(args, "displayId");
            blocking(move || crate::wallpaper::next(app, display)).await
        }
        "wallpaper_prev" => {
            let app = app.clone();
            let display = opt_str(args, "displayId");
            blocking(move || crate::wallpaper::prev(app, display)).await
        }
        "display_binding_set" => {
            let app = app.clone();
            let display = req_str(args, "displayId")?;
            let pid = args.get("playlistId").and_then(|v| v.as_i64());
            let bound = pid.is_some();
            blocking(move || crate::wallpaper::display_binding_set(app, display, pid)).await?;
            Ok(json!({ "bound": bound }))
        }
        "wallpaper_rotation_set" => {
            let app = app.clone();
            let paused = args
                .get("paused")
                .and_then(|v| v.as_bool())
                .ok_or("缺少 paused")?;
            blocking(move || crate::wallpaper::rotation_set(app, paused)).await?;
            Ok(json!({ "paused": paused }))
        }
        "active_items" => {
            let app = app.clone();
            let ids = blocking(move || crate::wallpaper::active_items(app)).await?;
            Ok(json!({ "items": ids }))
        }
        "library_list" => {
            let app = app.clone();
            let wtype = opt_str(args, "type");
            // tagGroups：组内并集、组间交集；`$project` / `$local` 两个库内专用值由
            // library.rs 翻译（前者真源是工程目录，后者是 custom-* / 引用模式条目）
            let tag_groups: Vec<Vec<String>> = args
                .get("tagGroups")
                .and_then(|v| v.as_array())
                .map(|groups| {
                    groups
                        .iter()
                        .filter_map(|g| g.as_array())
                        .map(|g| {
                            g.iter()
                                .filter_map(|t| t.as_str())
                                .map(|t| t.trim().to_string())
                                .filter(|t| !t.is_empty())
                                .collect::<Vec<_>>()
                        })
                        .filter(|g: &Vec<String>| !g.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let filter = Some(LibraryFilter {
                query: opt_str(args, "query"),
                sort: opt_str(args, "sort"),
                tag_groups,
                ..Default::default()
            });
            let items =
                blocking(move || crate::library::library_list_impl(app, wtype, filter)).await?;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(50)
                .min(500) as usize;
            let total = items.len();
            let page: Vec<Value> = items
                .into_iter()
                .skip(offset)
                .take(limit)
                .filter_map(|i| serde_json::to_value(i).ok())
                .collect();
            Ok(json!({ "total": total, "offset": offset, "limit": limit, "items": page }))
        }
        "library_delete" => {
            let app = app.clone();
            let item_id = req_str(args, "itemId")?;
            let removed = blocking(move || crate::library::library_delete(app, item_id)).await?;
            Ok(json!({ "deleted": removed }))
        }
        "library_remove" => {
            let app = app.clone();
            let item_id = req_str(args, "itemId")?;
            let removed = blocking(move || crate::library::library_remove(app, item_id)).await?;
            Ok(json!({ "removed": removed }))
        }
        "library_open_folder" => {
            let app = app.clone();
            let item_id = req_str(args, "itemId")?;
            let opened =
                blocking(move || crate::library::library_open_folder(app, item_id)).await?;
            Ok(json!({ "opened": opened }))
        }
        "item_props_get" => {
            let item_id = req_str(args, "itemId")?;
            let props = crate::library::item_props(app.clone(), item_id)?;
            Ok(serde_json::to_value(props).unwrap_or(json!([])))
        }
        "item_props_set" => {
            let app = app.clone();
            let item_id = req_str(args, "itemId")?;
            if opt_bool(args, "reset") {
                let id2 = item_id.clone();
                blocking(move || crate::library::reset_item_props(app, id2)).await?;
                return Ok(json!({ "itemId": item_id, "reset": true }));
            }
            let values = args
                .get("values")
                .and_then(|v| v.as_object())
                .cloned()
                .ok_or("缺少 values 对象（或传 reset=true 清除覆盖）")?;
            let id2 = item_id.clone();
            blocking(move || crate::library::set_item_props(app, id2, values)).await?;
            Ok(json!({ "itemId": item_id, "updated": true }))
        }
        "workshop_search" => {
            let svc = app
                .try_state::<std::sync::Arc<crate::workshop::WorkshopService>>()
                .ok_or("工坊服务未就绪")?
                .inner()
                .clone();
            let params: crate::steam::types::WorkshopSearchParams =
                serde_json::from_value(args.clone()).map_err(|e| format!("参数不合法: {e}"))?;
            let r = svc.search(params).await?;
            Ok(serde_json::to_value(r).unwrap_or(json!({})))
        }
        "workshop_item" => {
            let svc = app
                .try_state::<std::sync::Arc<crate::workshop::WorkshopService>>()
                .ok_or("工坊服务未就绪")?
                .inner()
                .clone();
            let id = req_str(args, "id")?;
            let r = svc.detail(&id).await?;
            Ok(serde_json::to_value(r).unwrap_or(json!(null)))
        }
        #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
        "workshop_upload" => {
            let app = app.clone();
            let project = req_str(args, "project")?;
            let r = crate::workshop_upload::workshop_upload_start(
                app,
                None,
                Some(project),
                opt_str(args, "title"),
                opt_str(args, "description"),
                args.get("tags").and_then(|v| v.as_array()).map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(|s| s.to_string()))
                        .collect()
                }),
                opt_str(args, "visibility"),
                opt_str(args, "changelog"),
            )
            .await?;
            Ok(serde_json::to_value(r).unwrap_or(json!({})))
        }
        #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
        "workshop_upload_status" => {
            let app = app.clone();
            let job_id = opt_str(args, "jobId");
            let r = crate::workshop_upload::workshop_upload_status(app, job_id).await?;
            Ok(serde_json::to_value(r).unwrap_or(json!({})))
        }
        "download_enqueue" => {
            let app = app.clone();
            let item_id = req_str(args, "itemId")?;
            let id = blocking(move || crate::download::download_enqueue(app, item_id)).await?;
            Ok(json!({ "queued": id }))
        }
        "download_list" => {
            let app = app.clone();
            let rows = blocking(move || crate::download::download_list(app)).await?;
            Ok(serde_json::to_value(rows).unwrap_or(json!([])))
        }
        "download_cancel" => {
            let app = app.clone();
            let id = req_i64(args, "id")?;
            let ok = blocking(move || crate::download::download_cancel(app, id)).await?;
            Ok(json!({ "cancelled": ok }))
        }
        "download_retry" => {
            let app = app.clone();
            let id = req_i64(args, "id")?;
            let ok = blocking(move || crate::download::download_retry(app, id)).await?;
            Ok(json!({ "retried": ok }))
        }
        other => Err(format!("未知工具: {other}")),
    }
}

/// 该条目是否**正是**当前挂在所有屏上、且渲染器已就绪的那张。
///
/// 三个条件都要满足才允许截图跳过 apply（省掉一次完整的 pkg 解析 + WebGL 重建）：
///   - 每面屏的会话配置都指向这个条目 —— 只比「最近 ready 的条目」是不够的：
///     刚切到 B、B 还在挂载时，`ready_item` 仍是上一次的 A，只比 ready 会拿 A 的旧判断
///     去跳过 B 的挂载，截到的就是张正在换台的画面。
///   - 渲染器最近一次 ready 的条目也是它（渲染器还没报过 ready 就不能算挂好）。
///   - **屏幕上的这份不比磁盘上的库文件旧**（见 [`item_content_stamp`]）。
///
/// 第三条是「改完看不到变化」的根治：agent 的迭代是
/// 改文件 → scene_pack → project_update，而 `project_update` **刻意保持 itemId 不变**
/// （用户改过的属性覆盖挂在 itemId 上），库导入路径也不会去碰壁纸引擎 —— 只比 itemId
/// 的话，第二轮迭代会被判成「还是那张、跳过应用」，截图和 preview.png 都停留在上一版。
fn already_ready(app: &AppHandle, item_id: &str) -> bool {
    let Some(state) = app.try_state::<crate::wallpaper::WallpaperEngineState>() else {
        return false;
    };
    let Ok(windows) = state.windows.lock() else {
        return false;
    };
    if windows.is_empty() {
        return false;
    }
    let all_match = windows
        .values()
        .all(|cfg| crate::wallpaper::item_id_of(cfg).as_deref() == Some(item_id));
    drop(windows);
    // 换纸还在飞时 windows 里已登记新配置、ready 还是上一张的 —— 都不作数，
    // 等换纸整体落地再判「已挂在屏上」
    if !all_match
        || crate::content_server::ready_item(app).as_deref() != Some(item_id)
        || crate::wallpaper::any_swap_in_flight()
    {
        return false;
    }
    // ready 时刻早于库文件的最新 mtime = 屏幕上这份是旧的（重新安装过但没重挂）
    let stamp = item_content_stamp(app, item_id);
    crate::system_wallpaper::ready_stamp(app) >= stamp
}

/// 库内条目的内容指纹（epoch 毫秒）：条目目录 + 几个关键文件的 mtime 取最大。
///
/// 只 stat 5 个路径，几百张壁纸的库里也不会成为负担；条目的重新安装会重建目录或
/// 覆盖 scene.pkg（`update_project` 先把旧目录 rename 走再导入），所以 mtime 一定会变。
fn item_content_stamp(app: &AppHandle, item_id: &str) -> u64 {
    let Ok(dir) = crate::library::item_dir(app, item_id) else {
        return 0;
    };
    let mut newest = 0u64;
    let mut bump = |p: &std::path::Path| {
        if let Ok(m) = std::fs::metadata(p).and_then(|m| m.modified()) {
            if let Ok(d) = m.duration_since(std::time::UNIX_EPOCH) {
                newest = newest.max(d.as_millis() as u64);
            }
        }
    };
    bump(&dir);
    for f in ["scene.pkg", "project.json", "scene.json", "index.html"] {
        bump(&dir.join(f));
    }
    newest
}

/// 拍一张：**先让渲染器自己抓帧**（三个平台同一套），失败再回退到平台原生快照
/// （macOS 有；web 类型只有它能拍）。
///
/// 返回 `(bytes, mime)`。
async fn capture_once(
    app: &AppHandle,
    t0: u64,
    settle: u64,
    timeout: u64,
    max_width: Option<u32>,
    // 只拍这块屏的窗口（多屏各挂不同壁纸时用；None = 自动挑一扇）
    only_label: Option<&str>,
) -> Result<(Vec<u8>, String), String> {
    match crate::system_wallpaper::capture_via_renderer(
        app, t0, settle, timeout, max_width, only_label,
    )
    .await
    {
        Ok((bytes, mime, _label)) => Ok((bytes, mime)),
        Err(js_err) => {
            // 页面抓不到（web 类型 / 画布还没准备好）→ 试平台原生。
            // 失败原因同时记进诊断：否则「回退成功了」这条路上，页面自抓帧为什么失败
            // 完全看不出来（页面的成功日志只到「正在回传」为止）。
            crate::content_server::note_diag(
                app,
                &format!("capture: 页面自抓帧失败，回退原生快照 —— {js_err}"),
                only_label,
            );
            match crate::system_wallpaper::capture_wallpaper_png(
                app, t0, settle, timeout, only_label,
            )
            .await
            {
                Ok(png) => {
                    let mime = image_mime_of(&png).to_string();
                    Ok((png, mime))
                }
                Err(native_err) => Err(format!("{js_err}；原生快照也不行：{native_err}")),
            }
        }
    }
}

/// 截图自检：可选先应用 → 等渲染就绪 → 实拍 → 可选存 preview.png
async fn screenshot(app: &AppHandle, args: &Value) -> Result<Value, String> {
    let item_id = req_str(args, "itemId")?;
    let apply = args.get("apply").and_then(|v| v.as_bool()).unwrap_or(true);
    // 多屏各挂不同壁纸时要能点名拍哪一块屏：displayId（displays_list 里那个 id）
    let display = opt_str(args, "displayId").filter(|d| !d.trim().is_empty());
    let only_label = display
        .as_deref()
        .map(crate::system_wallpaper::label_for_display);
    // force：即使「已挂在屏上」也重新应用。改完工程（同一个 itemId）想确保截到新版
    // 时用；正常迭代不必传 —— already_ready 已经会用内容时间来判新旧。
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let settle = args
        .get("settleMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(1500);
    // 默认给 60s：大 scene.pkg 冷启动解析常到 30s+，30s 上限会把「正在成功」的
    // 挂载判成超时（用户侧表现为同一张壁纸反复「等待渲染器就绪超时」）
    let timeout = args
        .get("timeoutMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(60_000);
    let save_preview = args
        .get("saveAsPreview")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    // 已经挂在屏上且渲染器已就绪 → 跳过重复应用。
    // 否则每次截图都会走一遍 setWallpaper → 库重新解析 pkg + 重建 WebGL 上下文，
    // 连拍时既慢又容易撞超时（这正是「截图超时」高频出现的另一个来源）。
    // 指定了屏就不走「已挂好」快路径：already_ready 判的是「所有屏都是这张」，
    // 与「只看这块屏」不是一回事，宁多重挂一次也不要拍错屏。
    let skip_apply = apply && !force && only_label.is_none() && already_ready(app, &item_id);
    // 先读 ready 时间戳再应用：否则可能立刻读到上一张壁纸的旧时间戳。
    // 不重新应用时 t0 一律置 0 —— 当前时间戳本就属于屏幕上这张，不该再等它「变得更
    // 新」（apply=false 只想拍「现在屏上是什么」，若还等新 ready 就必然等到超时，
    // 只能靠轮播换纸之类的偶然事件救场）。
    let t0 = if skip_apply || !apply {
        0
    } else {
        crate::system_wallpaper::ready_stamp(app)
    };
    if apply && !skip_apply {
        let app2 = app.clone();
        let id = item_id.clone();
        let disp = display.clone();
        blocking(move || crate::wallpaper::apply_item(app2, id, disp)).await?;
    }
    // 多帧：`framesMs` 给的是「相对首帧就绪」的毫秒时刻（升序去重、上限 8 个）。
    // 首帧仍走同一条等待路径（t0 判新旧 + settle），后续帧只是在已挂载的页面上
    // 按时间差再拍 —— 一帧一个 image 块，顺序与请求一致。
    let frames: Vec<u64> = match args.get("framesMs").and_then(|v| v.as_array()) {
        Some(arr) => {
            let mut v: Vec<u64> = arr.iter().filter_map(|x| x.as_u64()).collect();
            v.sort_unstable();
            v.dedup();
            if v.len() > 8 {
                return Err("framesMs 最多 8 个时刻（多了既慢又占上下文）".into());
            }
            v
        }
        None => Vec::new(),
    };
    // apply=false 表示「拍屏上现在这张」：那就必须确认屏上那张**就是**请求的这张。
    // 否则 A 挂在屏上时请求 B，会把 A 的画面当成 B 交出去 —— 而且看起来一切正常。
    if !apply {
        if let Some((label, onscreen)) =
            crate::system_wallpaper::onscreen_item(app, display.as_deref())
        {
            if onscreen != item_id {
                return Err(format!(
                    "屏幕上是 {onscreen}（窗口 {label}），不是请求的 {item_id}；\
                     要拍它请传 apply=true（会先应用），或先把它应用上去"
                ));
            }
        }
    }

    // 抓帧分辨率：截图只用于看效果/当封面，统一压到 1920 宽以内（也保证回传体积小）
    let max_width = args
        .get("maxWidth")
        .and_then(|v| v.as_u64())
        .unwrap_or(1920) as u32;
    let (png, mime) = capture_once(
        app,
        t0,
        settle,
        timeout,
        Some(max_width),
        only_label.as_deref(),
    )
    .await?;

    let mut images: Vec<Value> = vec![json!({
        "tMs": frames.first().copied().unwrap_or(0),
        "bytes": png.len(),
        "data": B64.encode(&png),
        "mimeType": mime,
    })];
    if frames.len() > 1 {
        let base = std::time::Instant::now();
        for t in frames.iter().skip(1) {
            // 第一帧拍摄本身已经花掉一段时间，按绝对时刻补齐剩余等待
            let elapsed = base.elapsed().as_millis() as u64;
            if *t > elapsed {
                tokio::time::sleep(std::time::Duration::from_millis(*t - elapsed)).await;
            }
            // t0=0：这张壁纸已经就绪，不该再等 ready 时间戳变新；settle 用调用方给的
            let (extra, extra_mime) = capture_once(
                app,
                0,
                settle,
                timeout,
                Some(max_width),
                only_label.as_deref(),
            )
            .await?;
            images.push(json!({
                "tMs": t,
                "bytes": extra.len(),
                "data": B64.encode(&extra),
                "mimeType": extra_mime,
            }));
        }
    }

    let mut saved_to = Value::Null;
    if save_preview {
        let project = match opt_str(args, "project") {
            Some(p) => Some(p),
            None => workspace::find_project_by_item(app, &item_id)
                .ok()
                .flatten(),
        };
        if let Some(project) = project {
            let app2 = app.clone();
            let bytes = png.clone();
            let ext = if mime.contains("jpeg") { "jpg" } else { "png" };
            let path =
                blocking(move || workspace::save_preview_as(&app2, &project, &bytes, ext)).await?;
            saved_to = json!(path);
        }
    }

    let mut out = json!({
        "itemId": item_id,
        "bytes": png.len(),
        "previewPath": saved_to,
        // 本次是否真的重新应用过（false = 复用了已经挂好的同一张壁纸）
        "applied": apply && !skip_apply,
    });
    if images.len() == 1 {
        // 单帧走老字段（`_image`）保持与既有客户端兼容。
        // ⚠️ mime 必须一起带上：老字段只放 base64，缺了它就只剩「按扩展名硬编码」这一条
        // 路，渲染器自抓帧产出的是 JPEG，客户端却会按 PNG 去解。
        let mut one = images.remove(0);
        out["mimeType"] = one["mimeType"].clone();
        out["_image"] = one["data"].take();
    } else {
        out["frames"] = json!(images
            .iter()
            .map(|f| json!({ "tMs": f["tMs"], "bytes": f["bytes"] }))
            .collect::<Vec<_>>());
        out["_images"] = json!(images);
    }
    Ok(out)
}

/// 离屏预览：让**主窗口**把这张壁纸挂在屏外的画布上渲染，再抓帧（可多帧）。
///
/// 为什么不是「另开一扇预览窗」：macOS 上不可见的窗口会被 WebKit 判为遮挡而停止出帧
/// —— 实测预览窗把 scene.pkg 全部解析完、贴图也传完了，却永远等不到首帧 ready
/// （诊断停在 mount 中途）。主窗口本来就可见在跑，把画布放到屏外既不露给用户、
/// 三个平台也同一套代码，还省掉了建窗/销毁/数据存储回收这一圈。
///
/// 桌面壁纸会话**完全不动**，所以比 `wallpaper_screenshot` 更适合「改一处看一眼」。
async fn preview(app: &AppHandle, args: &Value) -> Result<Value, String> {
    // itemId 优先；只给 project 时用它在本地库里的副本（没装过就先装）
    let item_id = match opt_str(args, "itemId") {
        Some(id) if !id.trim().is_empty() => id,
        _ => {
            let project = req_str(args, "project")?;
            match workspace::installed_item_id_of(app, &project) {
                Some(id) => id,
                None => {
                    let (id, _title, _ty) = workspace::install_project(app, &project)?;
                    id
                }
            }
        }
    };
    let width = args.get("width").and_then(|v| v.as_u64()).unwrap_or(1280);
    let height = args.get("height").and_then(|v| v.as_u64()).unwrap_or(720);
    let max_width = args
        .get("maxWidth")
        .and_then(|v| v.as_u64())
        .unwrap_or(width) as u32;
    let timeout = args
        .get("timeoutMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(60_000);
    let keep_open = opt_bool(args, "keepOpen");
    let frames: Vec<u64> = match args.get("framesMs").and_then(|v| v.as_array()) {
        Some(arr) => {
            let mut v: Vec<u64> = arr.iter().filter_map(|x| x.as_u64()).collect();
            v.sort_unstable();
            v.dedup();
            if v.len() > 8 {
                return Err("framesMs 最多 8 个时刻".into());
            }
            v
        }
        None => Vec::new(),
    };

    // 主窗口（label = "main"）：只有它装了 `__wpPreview` / `__wpCapture` 控制面。
    //
    // 主窗口可能被关掉、也可能被内存压力看门狗回收（见 main_window.rs）—— 那时
    // 预览就没有渲染面了。**不自动把它弹出来**（agent 迭代时每调一次就弹一次主界面太扰人），
    // 想要的话显式传 `openMainWindow: true`。
    let mut opened_main = false;
    if app.get_webview_window("main").is_none() {
        if !opt_bool(args, "openMainWindow") {
            return Err(
                "主窗口当前不存在（已关闭或被内存压力回收）—— 离屏预览需要一个**可见的渲染面**\
                 （隐藏/屏外窗口会被 WebKit 停帧，实测等不到首帧）。\
                 两条路：① 传 openMainWindow=true 让宿主重新打开主界面；\
                 ② 先在托盘/全局热键里打开主界面再重试；\
                 ③ 或改用 wallpaper_screenshot（它借壁纸窗口渲染，不需要主窗口）"
                    .into(),
            );
        }
        let app2 = app.clone();
        blocking(move || {
            crate::main_window::ensure_main_window(&app2);
            Ok(())
        })
        .await?;
        // 建窗是异步的：等它真的出现再派发指令
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while app.get_webview_window("main").is_none() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        opened_main = app.get_webview_window("main").is_some();
        if !opened_main {
            return Err("尝试打开主窗口失败（8s 内未出现）".into());
        }
    }
    let window = app
        .get_webview_window("main")
        .ok_or("主窗口不存在（预览依赖主窗口的离屏画布）")?;
    let js = format!(
        "window.__wpPreview && window.__wpPreview.start({}, {width}, {height})",
        serde_json::to_string(&item_id).unwrap_or_else(|_| "''".into())
    );
    window
        .eval(&js)
        .map_err(|e| format!("派发预览指令失败: {e}"))?;

    // 快速失败：预览桥是**前端**装的控制面，主窗口前端版本旧 / 还没加载完时上面那句
    // eval 会静默落空（`window.__wpPreview && …`）。没看到页面回话就别干等满超时。
    //
    // 等待时长分两种：主窗口本来就在 → 3s 足够（桥早装好了，不该有"静默落空"以外的情况）；
    // **刚被本工具打开** → 它的前端还在加载（dev 下还要拉 vite 的模块图），给 20s。
    {
        let wait = if opened_main { 20 } else { 3 };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait);
        let mut answered = false;
        while std::time::Instant::now() < deadline {
            if crate::content_server::diag_has_recent(app, "[preview] start", 8_000) {
                answered = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        }
        if !answered {
            return Err(format!(
                "主窗口的预览桥没有响应（等了 {wait}s）—— 前端可能是旧版本（刷新一下主窗口即可），\
                 或预览所需的 window.__wpPreview 未安装（见 src/lib/preview.ts）"
            ));
        }
    }

    // 抓帧：页面会等挂载落定再回图（超时/失败都会给原因）
    let mut images: Vec<Value> = Vec::new();
    let times: Vec<u64> = if frames.is_empty() { vec![0] } else { frames };
    let started = std::time::Instant::now();
    let result = async {
        for (i, t) in times.iter().enumerate() {
            if i > 0 {
                let elapsed = started.elapsed().as_millis() as u64;
                if *t > elapsed {
                    tokio::time::sleep(std::time::Duration::from_millis(*t - elapsed)).await;
                }
            }
            let (bytes, mime) = crate::content_server::request_capture(
                app,
                "main",
                Some(max_width),
                std::time::Duration::from_millis(timeout),
            )
            .await?;
            images.push(json!({
                "tMs": t,
                "bytes": bytes.len(),
                "data": B64.encode(&bytes),
                "mimeType": mime,
            }));
        }
        Ok::<(), String>(())
    }
    .await;

    if !keep_open {
        // 收掉离屏画布：不释放的话主窗口会替这张壁纸一直占着 WebGL 上下文与 pkg 缓存
        let _ = window.eval("window.__wpPreview && window.__wpPreview.stop()");
    }
    result?;

    let mut out = json!({
        "itemId": item_id,
        "size": { "width": width, "height": height },
        "keptOpen": keep_open,
        "openedMainWindow": opened_main,
        "frames": images
            .iter()
            .map(|f| json!({ "tMs": f["tMs"], "bytes": f["bytes"], "mimeType": f["mimeType"] }))
            .collect::<Vec<_>>(),
    });
    out["_images"] = json!(images);
    Ok(out)
}

/// 按关键词查踩坑清单（省 token：不用整份 pitfalls 都读进来）。
fn pitfall_search(args: &Value) -> Result<Value, String> {
    let q = opt_str(args, "query").unwrap_or_default();
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .max(1) as usize;
    let all = crate::mcp::references::pitfalls_json();
    let items = all["items"].as_array().cloned().unwrap_or_default();
    let needle = q.trim().to_lowercase();
    if needle.is_empty() {
        // 没给关键词 → 只给目录（id + 症状首句），让调用方再精查
        let index: Vec<Value> = items
            .iter()
            .map(|it| {
                json!({
                    "id": it["id"],
                    "症状": it["症状"].as_str().unwrap_or("").chars().take(70).collect::<String>(),
                })
            })
            .collect();
        return Ok(json!({ "total": items.len(), "index": index }));
    }
    let mut hits: Vec<Value> = Vec::new();
    for it in &items {
        let hay = serde_json::to_string(it).unwrap_or_default().to_lowercase();
        if hay.contains(&needle) {
            hits.push(it.clone());
        }
        if hits.len() >= limit {
            break;
        }
    }
    Ok(json!({
        "query": q,
        "matched": hits.len(),
        "total": items.len(),
        "items": hits,
        "hint": if hits.is_empty() {
            "没命中；可先不带 query 调用一次拿目录（id + 症状），或用更短的关键词"
        } else {
            "改法里提到的资源/工具都能直接调用：wallpaperem://reference/effects|particles、effect_scaffold、particle_recipe"
        },
    }))
}

/// 图层自检：**渲染一次 + 量该层矩形 + 交叉核对诊断** —— 判定「画出来没有、是不是白块、为什么」。
///
/// 为什么不用 A/B 差分：差分只能告诉你"有没有贡献"，说不出**原因**；而渲染库把
/// 「跳过效果（pass 编译失败）」这类信息交给了诊断通道。所以这里改成：
///   ① 按 cover 映射把图层矩形折算到截图像素；
///   ② 量矩形内的均值/标准差（白块 = 高均值 + 低方差）；
///   ③ 在 renderer_diag 里找与该层效果相关的跳过/编译失败记录；
///   ④ 给出 verdict + 证据 + 改法。
async fn layer_selfcheck(app: &AppHandle, args: &Value) -> Result<Value, String> {
    let project = req_str(args, "project")?;
    let target = req_str(args, "layer")?;
    let settle = args
        .get("settleMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(1000);
    let timeout = args
        .get("timeoutMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(60_000);
    let max_width = args
        .get("maxWidth")
        .and_then(|v| v.as_u64())
        .unwrap_or(1280) as u32;

    // 图层信息（name 或 id 定位）
    let dir = workspace::project_dir(app, &project)?;
    let scene_txt = std::fs::read_to_string(workspace::safe_join(&dir, "scene.json")?)
        .map_err(|e| format!("读不到 scene.json: {e}"))?;
    let scene: Value =
        serde_json::from_str(&scene_txt).map_err(|e| format!("scene.json 不是合法 JSON: {e}"))?;
    let objs = scene
        .get("objects")
        .and_then(|o| o.as_array())
        .ok_or("scene.json 里没有 objects 数组")?;
    let obj = objs
        .iter()
        .find(|o| o.get("name").and_then(|n| n.as_str()) == Some(target.as_str()))
        .or_else(|| {
            target.parse::<i64>().ok().and_then(|id| {
                objs.iter()
                    .find(|o| o.get("id").and_then(|v| v.as_i64()) == Some(id))
            })
        })
        .ok_or_else(|| {
            format!(
                "找不到图层「{target}」；现有图层：{}",
                objs.iter()
                    .filter_map(|o| o.get("name").and_then(|n| n.as_str()))
                    .collect::<Vec<_>>()
                    .join(" / ")
            )
        })?;
    let layer_name = obj
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or(&target)
        .to_string();
    let effects: Vec<String> = obj
        .get("effects")
        .and_then(|e| e.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| e.get("file").and_then(|f| f.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // 图层矩形（设计坐标，Y 向上、origin 是盒子中心）→ 屏幕像素
    let design: (f64, f64) = {
        let g = scene
            .get("general")
            .and_then(|g| g.get("orthogonalprojection"));
        (
            g.and_then(|o| o.get("width"))
                .and_then(|v| v.as_f64())
                .unwrap_or(1920.0),
            g.and_then(|o| o.get("height"))
                .and_then(|v| v.as_f64())
                .unwrap_or(1080.0),
        )
    };
    let num3 = |v: Option<&Value>| -> Option<[f64; 3]> {
        let v = v?;
        let s = match v {
            Value::String(s) => s.clone(),
            Value::Object(_) => v.get("value")?.as_str()?.to_string(),
            _ => return None,
        };
        // WE 里 vector 字段常写成两分量（"600 400"）或三分量，都要认（缺的补 0）
        let p: Vec<f64> = s
            .split_whitespace()
            .filter_map(|x| x.parse().ok())
            .collect();
        (p.len() >= 2).then(|| [p[0], p[1], *p.get(2).unwrap_or(&0.0)])
    };
    let origin = num3(obj.get("origin")).unwrap_or([design.0 / 2.0, design.1 / 2.0, 0.0]);
    let size = num3(obj.get("size")).unwrap_or([design.0, design.1, 0.0]);
    let scale = num3(obj.get("scale")).unwrap_or([1.0, 1.0, 1.0]);
    let half_w = (size[0] * scale[0]).abs() / 2.0;
    let half_h = (size[1] * scale[1]).abs() / 2.0;

    // 应用到桌面并抓一帧
    let item = match workspace::installed_item_id_of(app, &project) {
        Some(id) => id,
        None => workspace::install_project(app, &project)?.0,
    };
    let t0 = crate::system_wallpaper::ready_stamp(app);
    {
        let app_a = app.clone();
        let item_a = item.clone();
        blocking(move || crate::wallpaper::apply_item(app_a, item_a, None)).await?;
    }
    let (bytes, _mime) = capture_once(app, t0, settle, timeout, Some(max_width), None).await?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("截图解码失败: {e}"))?
        .to_rgb8();
    let (w, h) = img.dimensions();

    // cover 映射：设计高度铺满、宽度居中裁切（size 的宽高比与贴图不同的层会有偏差，
    // 这里只用于"取一块区域做统计"，不要求像素级精确）
    let view_w = design.1 * (w as f64) / (h as f64);
    let view_left = design.0 / 2.0 - view_w / 2.0;
    let to_px = |dx: f64| ((dx - view_left) / view_w * w as f64).round() as i64;
    let to_py = |dy: f64| ((1.0 - dy / design.1) * h as f64).round() as i64;
    let (x0, x1) = (to_px(origin[0] - half_w), to_px(origin[0] + half_w));
    let (y0, y1) = (to_py(origin[1] + half_h), to_py(origin[1] - half_h));
    let (cx0, cx1) = (x0.max(0).min(w as i64), x1.max(0).min(w as i64));
    let (cy0, cy1) = (y0.max(0).min(h as i64), y1.max(0).min(h as i64));
    let offscreen = cx1 - cx0 < 4 || cy1 - cy0 < 4;

    // 区域内统计
    let mut n = 0u64;
    let (mut sr, mut sg, mut sb) = (0u64, 0u64, 0u64);
    let (mut s2, mut s2g, mut s2b) = (0u64, 0u64, 0u64);
    for y in cy0..cy1 {
        for x in cx0..cx1 {
            let p = img.get_pixel(x as u32, y as u32);
            n += 1;
            sr += p[0] as u64;
            sg += p[1] as u64;
            sb += p[2] as u64;
            s2 += (p[0] as u64).pow(2);
            s2g += (p[1] as u64).pow(2);
            s2b += (p[2] as u64).pow(2);
        }
    }
    let avg = |s: u64| if n > 0 { s as f64 / n as f64 } else { 0.0 };
    let std = |s: u64, s2v: u64| {
        if n > 0 {
            ((s2v as f64 / n as f64) - (s as f64 / n as f64).powi(2))
                .max(0.0)
                .sqrt()
        } else {
            0.0
        }
    };
    let (mr, mg, mb) = (avg(sr), avg(sg), avg(sb));
    let (dr, dg, db) = (std(sr, s2), std(sg, s2g), std(sb, s2b));
    let max_std = dr.max(dg).max(db);
    let mean_all = (mr + mg + mb) / 3.0;

    // 交叉核对诊断：渲染库把「跳过效果」也报进来了
    let diag_hits: Vec<String> = crate::content_server::diag_snapshot(
        app,
        &crate::content_server::DiagQuery {
            label: None,
            item: None,
            since_ms: 0,
            limit: 60,
            clear: false,
        },
    )
    .get("entries")
    .and_then(|e| e.as_array())
    .map(|arr| {
        arr.iter()
            .filter_map(|e| e.get("msg").and_then(|m| m.as_str()))
            .filter(|m| {
                (m.contains("跳过效果") || m.contains("编译失败") || m.contains("failed"))
                    && (effects.iter().any(|f| {
                        let stem = f.trim_start_matches("effects/").trim_end_matches(".json");
                        m.contains(stem)
                    }) || effects.is_empty())
            })
            .map(|m| m.rsplit("] ").next().unwrap_or(m).to_string())
            .collect()
    })
    .unwrap_or_default();

    let verdict = if offscreen {
        "offscreen"
    } else if !diag_hits.is_empty() {
        "effect_skipped"
    } else if mean_all > 230.0 && max_std < 30.0 {
        "flat_white"
    } else if max_std < 6.0 {
        "flat_color"
    } else if n > 0 && mean_all < 3.0 {
        "blank"
    } else {
        "content"
    };
    let advice = match verdict {
        "offscreen" => "该层矩形几乎不在可见区内（cover 下左右会被裁掉）：换 origin 或缩小 size 再看",
        "effect_skipped" => "效果被渲染库跳过了（诊断里有记录，见 evidence）：按 pitfall_search「.vert」「白块」逐条核对四件套是否齐全",
        "flat_white" => "纯白块 —— 典型「内置材质被画出来、自写 shader 没跑」。检查 shaders/effects/<名>.{frag,vert} 是否成对、effects json 是否用 material 字段",
        "flat_color" => "一片纯色、没有结构：可能 shader 只输出了常量，或贴图/uv 用错",
        "blank" => "区域内几乎全黑：该层没画出来（被跳过或尺寸为 0）",
        _ => "区域内有结构，看起来正常画出了内容",
    };
    Ok(json!({
        "project": project,
        "layer": layer_name,
        "layerId": obj.get("id").cloned().unwrap_or(Value::Null),
        "effects": effects,
        "verdict": verdict,
        "advice": advice,
        "rect": {
            "design": { "x": [origin[0] - half_w, origin[0] + half_w], "y": [origin[1] - half_h, origin[1] + half_h] },
            "pixels": [cx0, cy0, cx1, cy1],
            "image": [w, h],
            "clipped": [x0, y0, x1, y1],
        },
        "regionStats": {
            "mean": [mr.round(), mg.round(), mb.round()],
            "std": [dr.round(), dg.round(), db.round()],
            "sampled": n,
        },
        "evidence": diag_hits,
        "note": "只渲染一次量该层矩形 + 交叉核对诊断（回答「这块看起来是什么」与「日志说了什么」，不回答「有没有贡献」）。矩形按 fit=cover 折算；若该层被其它层盖住，统计会包含上层内容。",
    }))
}

/// 一套现成粒子配方 → 两个文件的内容（预设 + 材质），贴图用渲染库内置程序化贴图。
fn particle_recipe(args: &Value) -> Result<Value, String> {
    let kind = req_str(args, "kind")?;
    let name = opt_str(args, "name").unwrap_or_else(|| kind.clone());
    let refs = crate::mcp::references::particles_json();
    let recipe = refs
        .get("recipes")
        .and_then(|r| r.get(&kind))
        .ok_or_else(|| {
            format!(
                "未知的 kind「{kind}」；可用：{}",
                refs["recipes"]
                    .as_object()
                    .map(|o| o.keys().cloned().collect::<Vec<_>>().join(" / "))
                    .unwrap_or_default()
            )
        })?;
    let mut preset = recipe["preset"].clone();
    if let Some(n) = args.get("maxCount").and_then(|v| v.as_u64()) {
        preset["maxcount"] = json!(n);
    }
    if let Some(r) = args.get("rate").and_then(|v| v.as_f64()) {
        if let Some(em) = preset.get_mut("emitter").and_then(|e| e.as_array_mut()) {
            if let Some(first) = em.first_mut() {
                first["rate"] = json!(r);
            }
        }
    }
    let preset_path = format!("particles/presets/{name}.json");
    let material_path = format!("materials/presets/{name}.json");
    preset["material"] = json!(material_path);
    if preset
        .get("controlpoint")
        .map(|c| c.is_string())
        .unwrap_or(false)
    {
        preset["controlpoint"] = json!((0..8)
            .map(|i| json!({ "flags": 0, "id": i, "offset": "0 0 0" }))
            .collect::<Vec<_>>());
    }
    let texture = recipe["texture"].as_str().unwrap_or("particle/halo_1");
    let material = json!({ "passes": [{
        "shader": "genericparticle", "blending": "additive", "cullmode": "nocull",
        "depthtest": "disabled", "depthwrite": "disabled", "textures": [texture],
    }] });
    Ok(json!({
        "kind": kind,
        "texture": texture,
        "purpose": recipe["用途"],
        "files": [
            { "path": preset_path, "content": serde_json::to_string_pretty(&preset).unwrap_or_default() },
            { "path": material_path, "content": serde_json::to_string_pretty(&material).unwrap_or_default() },
        ],
        "layer": {
            "particle": preset_path,
            "origin": "960 540 0",
            "size": "1920 1080",
            "note": "预设内的 emitter 坐标是层内局部坐标；整体位置/大小靠图层的 origin/size（Y 轴向上、origin 是盒子中心）",
        },
        "next": "把 files 里的两项用 project_write_file 落盘，再把 layer 片段贴进 scene.json 的 objects，然后 project_validate → scene_pack",
        "reference": "wallpaperem://reference/particles",
    }))
}

/// 自写效果 shader 脚手架：效果 json + 材质 json + .frag + .vert（四件套，缺一不可）。
///
/// 两种挂法：
///   - 普通效果层：models/util/solidlayer.json（内置纯色层）+ 加法混合，shader 自己输出颜色
///   - 全屏后期层（post_bloom / film_grain）：models/util/fullscreenlayer.json，
///     内容 = 当前已渲染画面（g_Texture0），**图层 alpha 即强度**
fn effect_scaffold(args: &Value) -> Result<Value, String> {
    let kind = req_str(args, "kind")?;
    let name = opt_str(args, "name").unwrap_or_else(|| kind.clone());
    let refs = crate::mcp::references::effects_json();
    let frag = match kind.as_str() {
        "water_ripple" => include_str!("templates/water_ripple.frag"),
        "pointer_aura" => include_str!("templates/pointer_aura.frag"),
        "audio_bars" => include_str!("templates/audio_bars.frag"),
        "clock" => include_str!("templates/clock.frag"),
        "post_bloom" => include_str!("templates/post_bloom.frag"),
        "film_grain" => include_str!("templates/film_grain.frag"),
        other => {
            return Err(format!(
                "未知的 kind「{other}」；可用：water_ripple / pointer_aura / audio_bars / clock / post_bloom / film_grain"
            ))
        }
    };
    // 后期类：全屏后期层 + normal 混合，强度交给图层 alpha
    let post = kind == "post_bloom" || kind == "film_grain";
    let vert = refs["vertexTemplate"].as_str().unwrap_or("").to_string();
    let effect = json!({ "passes": [{
        "material": format!("materials/effects/{name}.json"), "target": null, "bind": [],
    }] });
    let material = json!({ "passes": [{
        "shader": format!("effects/{name}"), "blending": "normal", "cullmode": "nocull",
        "depthtest": "disabled", "depthwrite": "disabled",
    }] });
    let layer = if post {
        json!({
            "image": "models/util/fullscreenlayer.json",
            "effects": [{ "file": format!("effects/{name}.json"), "name": name, "visible": true }],
            "origin": "960 540 0", "size": "1920 1080", "colorBlendMode": 0, "alpha": 0.6,
            "note": "fullscreenlayer = 全屏后期层（内容 = 当前已渲染画面），g_Texture0 即画面；                     它的**图层 alpha 就是强度**（与下层混合）→ 绑个 slider 属性就是强度滑条。                     必须放在 objects 的**最后**（读的是它下面的画面）",
        })
    } else {
        json!({
            "image": "models/util/solidlayer.json",
            "effects": [{ "file": format!("effects/{name}.json"), "name": name, "visible": true }],
            "origin": "960 540 0", "size": "1920 1080", "colorBlendMode": 9, "alpha": 1.0,
            "note": "solidlayer = 内置纯色层（工程里不用带文件）。shader 自己输出颜色、忽略 g_Texture0；                     要在图层四边乘 margin 淡出，否则边界会留亮线",
        })
    };
    let mut out = json!({
        "kind": kind,
        "files": [
            { "path": format!("effects/{name}.json"),
              "content": serde_json::to_string_pretty(&effect).unwrap_or_default() },
            { "path": format!("materials/effects/{name}.json"),
              "content": serde_json::to_string_pretty(&material).unwrap_or_default() },
            { "path": format!("shaders/effects/{name}.frag"), "content": frag },
            { "path": format!("shaders/effects/{name}.vert"), "content": vert },
        ],
        "layer": layer,
        "mustKnow": [
            "effects/<名>.json 的 passes[] 只写 material；shader 写在材质里（直写 shader 会被当命令 pass → 整条链静默不画）",
            "shaders/effects/<名>.frag 与 .vert **必须成对**：缺 .vert 会被静默跳过，图层只剩纯色块",
            "可用 uniform：g_Time / g_Daytime / g_PointerPosition(Last,State) / g_AudioSpectrum16/32/64Left+Right / g_Color / g_Alpha / g_Brightness / g_Texture0 / g_TexelSize / g_ModelViewProjectionMatrix",
        ],
        "next": "四个文件全部落盘（缺一个就静默失效）→ 图层片段贴进 scene.json → project_validate（会替你检查 .vert 是否成对）→ scene_pack → wallpaper_preview 看效果；不确定有没有生效就用 layer_selfcheck",
        "reference": "wallpaperem://reference/effects",
    });
    if opt_str(args, "layer").is_some() {
        out["layerSnippet"] = out["layer"].clone();
    }
    Ok(out)
}

fn list_sessions(app: &AppHandle) -> Result<Value, String> {
    let Some(state) = app.try_state::<crate::wallpaper::WallpaperEngineState>() else {
        return Err("壁纸引擎未就绪".into());
    };
    let windows: std::collections::HashMap<String, crate::wallpaper::WallpaperConfig> =
        state.windows.lock().map_err(|e| e.to_string())?.clone();
    let paused = *state.paused.lock().map_err(|e| e.to_string())?;
    Ok(json!({
        "active": !windows.is_empty(),
        "paused": paused,
        "sessions": windows,
    }))
}

/// 本地库单条查询（MCP resource `wallpaperem://library/{itemId}` 用）
pub fn library_item_json(app: &AppHandle, item_id: &str) -> Result<Value, String> {
    let db = app
        .try_state::<std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>>()
        .ok_or("DB 未就绪")?;
    let conn = db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT item_id, title, type, size_bytes, file_count, downloaded_at
             FROM library_items WHERE item_id = ?1",
        )
        .map_err(|e| e.to_string())?;
    // 先把行读成普通值再离开借用作用域：rusqlite 的 Rows 借用着 stmt
    let row = {
        let mut rows = stmt.query([item_id]).map_err(|e| e.to_string())?;
        let Some(row) = rows.next().map_err(|e| e.to_string())? else {
            return Err(format!("本地库里没有条目: {item_id}"));
        };
        (
            row.get::<_, String>(0).unwrap_or_default(),
            row.get::<_, String>(1).unwrap_or_default(),
            row.get::<_, String>(2).unwrap_or_default(),
            row.get::<_, i64>(3).unwrap_or(0),
            row.get::<_, i64>(4).unwrap_or(0),
            row.get::<_, i64>(5).unwrap_or(0),
        )
    };
    let (id, title, wtype, size_bytes, file_count, downloaded_at) = row;
    let dir = crate::library::item_dir(app, &id)?;
    Ok(json!({
        "itemId": id,
        "title": title,
        "type": wtype,
        "sizeBytes": size_bytes,
        "fileCount": file_count,
        "downloadedAt": downloaded_at,
        "filesPresent": crate::library::item_files_exist(&dir),
        "dir": dir.to_string_lossy(),
    }))
}

// ---------------------------------------------------------------- 小工具

fn text_content(v: &Value) -> Vec<Value> {
    let text = match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    };
    vec![json!({ "type": "text", "text": text })]
}

/// 多帧截图结果里的 `_images`（`[{data, mimeType}]`）
pub fn extract_images(result: &Value) -> Vec<(String, String)> {
    result
        .get("_images")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    let d = f.get("data")?.as_str()?.to_string();
                    let m = f
                        .get("mimeType")
                        .and_then(|m| m.as_str())
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| {
                            let head = &d[..d.len().min(8)];
                            let mut buf = Vec::with_capacity(6);
                            B64.decode_vec(head, &mut buf).ok();
                            image_mime_of(&buf).to_string()
                        });
                    Some((d, m))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 按魔术字节判图片格式：拿不到「生产者声明」时的兜底。
///
/// 为什么需要：`_image` 这条兼容通道只传 base64，不传 mime —— 声明与实际不一致时
/// 客户端按声明去解码就会失败（曾经把渲染器自抓帧的 **JPEG** 一律标成 `image/png`）。
pub fn image_mime_of(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.starts_with(b"RIFF") && bytes.len() >= 12 && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else {
        "image/png"
    }
}

/// 截图结果里的 `_image`（base64）要转成 MCP 的 image 内容块，只取一次。
///
/// mime 优先取结果里的声明（`mimeType`），没有就按前几个字节猜 —— 不再硬编码 png。
pub fn extract_image(result: &Value) -> Option<(String, String)> {
    let b64 = result.get("_image").and_then(|v| v.as_str())?;
    let mime = result
        .get("mimeType")
        .and_then(|m| m.as_str())
        .map(|m| m.to_string())
        .unwrap_or_else(|| {
            // 只解前 8 个 base64 字符（6 字节）就够判魔术字节
            let head = &b64[..b64.len().min(8)];
            let n = head.len() / 4 * 3;
            let mut buf = Vec::with_capacity(n);
            B64.decode_vec(head, &mut buf).ok();
            image_mime_of(&buf).to_string()
        });
    Some((b64.to_string(), mime))
}

fn req_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("缺少参数 {key}（需要非空字符串）"))
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn opt_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn req_i64(args: &Value, key: &str) -> Result<i64, String> {
    args.get(key)
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("缺少参数 {key}（需要整数）"))
}

async fn blocking<T, F>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

/// 把 Map 里的键排序（部分客户端要求稳定的字段顺序，纯锦上添花）
#[allow(dead_code)]
fn sorted_map(m: &Map<String, Value>) -> Vec<(String, Value)> {
    let mut v: Vec<(String, Value)> = m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 资源锁：同键同锁、异键异锁；键按字典序（保证加锁顺序一致、不死锁）
    #[test]
    fn resource_locks_are_per_key() {
        let a = resource_lock("project:p1");
        let b = resource_lock("project:p1");
        let c = resource_lock("project:p2");
        assert!(std::sync::Arc::ptr_eq(&a, &b), "同键必须是同一把锁");
        assert!(!std::sync::Arc::ptr_eq(&a, &c), "不同键不能共用一把锁");
    }

    /// 锁原语本身：同键互斥、异键不互等（用确定性等待证明，不靠"跑得慢"来观察）
    #[tokio::test]
    async fn same_key_serializes_and_other_keys_do_not() {
        use std::time::{Duration, Instant};
        let held = resource_lock("item:probe").lock_owned().await;
        let t0 = Instant::now();
        let waiter = tokio::spawn(async move {
            let _g = resource_lock("item:probe").lock_owned().await;
            Instant::now()
        });
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(!waiter.is_finished(), "同键任务不该在锁释放前就跑完");
        drop(held);
        let acquired = waiter.await.unwrap();
        assert!(
            acquired.duration_since(t0) >= Duration::from_millis(100),
            "同键必须排队（实测 {:?}）",
            acquired.duration_since(t0)
        );

        // 异键：互不阻塞（同一个工程域与条目域之间也如此）
        let _a = resource_lock("item:x").lock_owned().await;
        let t1 = Instant::now();
        let _b = resource_lock("item:y").lock_owned().await;
        let _c = resource_lock("project:z").lock_owned().await;
        assert!(t1.elapsed() < Duration::from_millis(50), "不同键不该互相等");
    }

    /// 锁键的覆盖范围：工程域锁工程；安装/更新**同时**锁工程与库条目；
    /// 预览/截图/删除锁库条目；两者顺序固定（字典序 → item 在前）。
    #[test]
    fn lock_keys_cover_both_namespaces() {
        assert_eq!(
            lock_keys("scene_pack", &json!({ "project": " p1 " })),
            vec!["item:p1".to_string(), "project:p1".to_string()],
            "打包会写库（install=true），两边都要锁；工程名要去空白"
        );
        assert_eq!(
            lock_keys("scene_inspect", &json!({ "project": "p1" })),
            vec!["project:p1".to_string()]
        );
        assert_eq!(
            lock_keys("wallpaper_preview", &json!({ "itemId": "custom-x" })),
            vec!["item:custom-x".to_string()]
        );
        assert_eq!(
            lock_keys("wallpaper_screenshot", &json!({ "itemId": "7" })),
            vec!["item:7".to_string()]
        );
        assert_eq!(
            lock_keys("library_delete", &json!({ "itemId": "7" })),
            vec!["item:7".to_string()]
        );
        // 只给工程名（"改完直接看"那条路）也必须锁到库条目 —— 否则预览与安装会撞车
        assert_eq!(
            lock_keys("wallpaper_preview", &json!({ "project": "p1" })),
            vec!["item:p1".to_string()],
            "预览只给 project 时也要按工程推出的条目 id 加锁"
        );
        assert_eq!(
            lock_keys("project_install", &json!({ "project": "p1" })),
            vec!["item:p1".to_string(), "project:p1".to_string()]
        );
        // 不同工程/条目互不阻塞；非资源域工具（如列表、播放控制）完全不锁
        assert_ne!(
            lock_keys("scene_pack", &json!({ "project": "p1" })),
            lock_keys("scene_pack", &json!({ "project": "p2" }))
        );
        assert!(lock_keys("projects_list", &json!({ "project": "p1" })).is_empty());
        assert!(lock_keys("wallpaper_apply", &json!({ "itemId": "7" })).is_empty());
        assert!(lock_keys("scene_pack", &json!({})).is_empty());
    }

    /// 图片 mime：按魔术字节判，别再靠硬编码
    #[test]
    fn image_mime_sniffs_magic_bytes() {
        assert_eq!(image_mime_of(&[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
        assert_eq!(image_mime_of(&[0x89, b'P', b'N', b'G', 0x0D]), "image/png");
        assert_eq!(image_mime_of(b"RIFFaaaaWEBPVP8 "), "image/webp");
        assert_eq!(image_mime_of(b"GIF89a"), "image/gif");
        assert_eq!(image_mime_of(b"????"), "image/png", "认不出时退回 png");
    }

    /// 单帧截图（老字段 `_image`）必须带上真实 mime：
    /// 声明优先；没有声明就按 base64 头几个字节嗅探（渲染器自抓帧给的是 JPEG）
    #[test]
    fn single_frame_image_keeps_real_mime() {
        let jpeg = B64.encode([0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46]);
        let declared = json!({ "_image": jpeg, "mimeType": "image/jpeg" });
        assert_eq!(
            extract_image(&declared),
            Some((jpeg.clone(), "image/jpeg".to_string()))
        );
        // 没声明 → 按字节判出 jpeg（旧实现固定回 image/png，客户端按 png 解会失败）
        let sniffed = json!({ "_image": jpeg });
        assert_eq!(extract_image(&sniffed).unwrap().1, "image/jpeg");
        // 真 PNG 仍然报 png
        let png = B64.encode([0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(
            extract_image(&json!({ "_image": png })).unwrap().1,
            "image/png"
        );
    }

    /// 结构化输出：对象/数组才给，标量与坏 JSON 不给
    #[test]
    fn structured_content_only_for_json_containers() {
        let obj = vec![json!({ "type": "text", "text": r#"{"a":1}"# })];
        assert_eq!(structured_content(&obj), Some(json!({ "a": 1 })));
        let arr = vec![json!({ "type": "text", "text": "[1,2]" })];
        assert_eq!(structured_content(&arr), Some(json!([1, 2])));
        let scalar = vec![json!({ "type": "text", "text": "ok" })];
        assert_eq!(structured_content(&scalar), None);
        let bad = vec![json!({ "type": "text", "text": "{not json" })];
        assert_eq!(structured_content(&bad), None);
        // 图片块在前、文本块在后也要能找到
        let with_img = vec![
            json!({ "type": "image", "data": "x", "mimeType": "image/png" }),
            json!({ "type": "text", "text": r#"{"ok":true}"# }),
        ];
        assert_eq!(structured_content(&with_img), Some(json!({ "ok": true })));
    }

    /// 声明了 outputSchema 的工具名必须真实存在，且 schema 是宽松的 object
    #[test]
    fn output_schemas_match_tool_names() {
        let defs = definitions();
        let names: Vec<String> = defs
            .iter()
            .filter_map(|d| d["name"].as_str().map(String::from))
            .collect();
        let mut declared = 0;
        for d in &defs {
            let Some(schema) = d.get("outputSchema") else {
                continue;
            };
            declared += 1;
            let name = d["name"].as_str().unwrap();
            assert!(names.contains(&name.to_string()));
            assert_eq!(
                schema["type"],
                json!("object"),
                "{name} 的 outputSchema 不是 object"
            );
            assert_eq!(
                schema["additionalProperties"],
                json!(true),
                "{name} 的 outputSchema 要允许附加字段（未来加字段不该变成破坏性变更）"
            );
            assert!(
                schema["properties"].is_object(),
                "{name} 的 outputSchema 缺少 properties"
            );
        }
        assert!(declared >= 8, "声明了 outputSchema 的工具太少: {declared}");
        // 反向：常用工具都应该声明
        for want in [
            "project_validate",
            "scene_inspect",
            "scene_pack",
            "renderer_diag",
        ] {
            assert!(
                defs.iter()
                    .any(|d| d["name"] == json!(want) && d.get("outputSchema").is_some()),
                "{want} 应当声明 outputSchema"
            );
        }
    }

    /// 工具清单与 call_inner 的 match 分派必须一一对应：
    /// 清单里有、分派里没有 = 客户端调这个工具永远得到「未知工具」。
    /// 没有活的 Tauri AppHandle 就没法真的调 call_inner（业务全在既有模块里），
    /// 所以这里直接对着本文件的分派臂文本核对。
    #[test]
    fn every_advertised_tool_is_dispatched() {
        let src = include_str!("tools.rs");
        let defs = definitions();
        assert!(defs.len() >= 24, "工具数量异常: {}", defs.len());

        let mut seen = HashSet::new();
        for d in &defs {
            let name = d["name"].as_str().expect("工具缺少 name");
            assert!(seen.insert(name.to_string()), "工具名重复: {name}");
            assert!(
                !d["description"].as_str().unwrap_or("").is_empty(),
                "{name} 缺少 description"
            );
            assert!(
                src.contains(&format!("\"{name}\" =>")),
                "工具 {name} 在 tools/list 里声明了，但 call_inner 没有对应分支"
            );

            // inputSchema 必须是合法的 object schema，且 required 字段都在 properties 里；
            // 写错的 schema 会让客户端在调用前就构造不出参数。
            let schema = &d["inputSchema"];
            assert_eq!(
                schema["type"],
                json!("object"),
                "{name} 的 inputSchema 不是 object"
            );
            let props = schema["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{name} 的 properties 不是对象"));
            // required 可以缺省（可选参数全不用写），但写了就必须是数组 ——
            // 写成对象（如 `json!({})`）在严格客户端那边是非法 schema。
            let required = match schema.get("required") {
                None => &Vec::new(),
                Some(v) => v
                    .as_array()
                    .unwrap_or_else(|| panic!("{name} 的 required 不是数组: {v}")),
            };
            for r in required {
                let r = r.as_str().unwrap_or_default();
                assert!(
                    props.contains_key(r),
                    "{name} 的 required 字段 {r} 不在 properties 里"
                );
            }
        }
    }
}
