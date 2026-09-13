//! MCP 工具实现：每个工具都是既有能力（workspace / library / wallpaper / workshop）
//! 的一层薄封装 —— 这里不重复实现任何业务逻辑，只做参数解析与结果整形。

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::{json, Map, Value};
use tauri::{AppHandle, Manager};

use crate::library::LibraryFilter;
use crate::workspace;

/// 工具清单（name / description / inputSchema）
pub fn definitions() -> Vec<Value> {
    let obj = |props: Value, required: Value| json!({ "type": "object", "properties": props, "required": required });
    vec![
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
            "description": "写工程内文件（自动建目录）。贴图等二进制内容用 encoding=base64。写入会让旧的 scene.pkg 失效并被删除。",
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
            "description": "把 scene 工程打包成 scene.pkg（materials 下的 PNG/JPEG 自动转成 .tex 并内嵌）。打包前会先校验，有 error 会拒绝打包。",
            "inputSchema": obj(json!({ "project": { "type": "string" } }), json!(["project"])),
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
            "description": "把本地库里的壁纸应用到桌面（所有显示器）。",
            "inputSchema": obj(json!({ "itemId": { "type": "string" } }), json!(["itemId"])),
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
            "description": "对当前壁纸窗口实拍一张 PNG 并作为图片内容返回（仅 macOS）。默认先应用到桌面再看效果；可顺手存成工程的 preview.png 当封面。同一张壁纸已经挂在屏上时会跳过重复应用（不会把大 scene.pkg 再解析一遍），连拍很快。",
            "inputSchema": obj(json!({
                "itemId": { "type": "string", "description": "要截图的本地库条目 id" },
                "apply": { "type": "boolean", "description": "是否先应用该壁纸，默认 true；该壁纸已就绪时自动跳过重复应用" },
                "settleMs": { "type": "integer", "description": "渲染就绪后额外等待毫秒数，默认 1500" },
                "timeoutMs": { "type": "integer", "description": "等待渲染就绪的上限，默认 60000（大 scene.pkg 冷启动可能要 30s+）" },
                "saveAsPreview": { "type": "boolean", "description": "是否把截图写成工程的 preview.png，默认 true" },
                "project": { "type": "string", "description": "可选：工程名（存 preview 用；不传则按 itemId 反查）" },
            }), json!(["itemId"])),
        }),
        json!({
            "name": "list_sessions",
            "description": "当前各屏的壁纸会话（label → 配置）与全局暂停态。",
            "inputSchema": obj(json!({}), json!([])),
        }),
        json!({
            "name": "active_items",
            "description": "当前已应用的本地库条目 id 列表。",
            "inputSchema": obj(json!({}), json!([])),
        }),
        json!({
            "name": "library_list",
            "description": "列出本地库壁纸（已安装/已下载），支持类型、标题关键字与排序。",
            "inputSchema": obj(json!({
                "type": { "type": "string", "description": "video / scene / web / gif，留空不过滤" },
                "query": { "type": "string", "description": "标题模糊搜索" },
                "sort": { "type": "string", "description": "downloaded_desc（默认）/ downloaded_asc / title_asc / title_desc / size_desc / size_asc" },
                "limit": { "type": "integer", "description": "默认 50，上限 500" },
                "offset": { "type": "integer", "description": "默认 0" },
            }), json!([])),
        }),
        json!({
            "name": "library_delete",
            "description": "从本地库删除壁纸（含磁盘文件与数据库记录，不可撤销）。",
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
    ]
}

// ---------------------------------------------------------------- 调用入口

pub async fn call(app: &AppHandle, name: &str, args: &Value) -> (Vec<Value>, bool) {
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
        "scene_pack" => {
            let app = app.clone();
            let project = req_str(args, "project")?;
            blocking(move || workspace::pack_scene(&app, &project)).await
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
            let id2 = item_id.clone();
            blocking(move || crate::wallpaper::apply_item(app, id2)).await?;
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
        "wallpaper_screenshot" => screenshot(app, args).await,
        "list_sessions" => list_sessions(app),
        "active_items" => {
            let app = app.clone();
            let ids = blocking(move || crate::wallpaper::active_items(app)).await?;
            Ok(json!({ "items": ids }))
        }
        "library_list" => {
            let app = app.clone();
            let wtype = opt_str(args, "type");
            let filter = Some(LibraryFilter {
                query: opt_str(args, "query"),
                sort: opt_str(args, "sort"),
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
        "library_open_folder" => {
            let app = app.clone();
            let item_id = req_str(args, "itemId")?;
            let opened = blocking(move || crate::library::library_open_folder(app, item_id)).await?;
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
        "workshop_upload" => {
            let app = app.clone();
            let project = req_str(args, "project")?;
            let r = crate::workshop_upload::workshop_upload_start(
                app,
                None,
                Some(project),
                opt_str(args, "title"),
                opt_str(args, "description"),
                args.get("tags")
                    .and_then(|v| v.as_array())
                    .map(|a| {
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
/// 两个条件都要满足才允许截图跳过 apply（省掉一次完整的 pkg 解析 + WebGL 重建）：
///   - 每面屏的会话配置都指向这个条目 —— 只比「最近 ready 的条目」是不够的：
///     刚切到 B、B 还在挂载时，`ready_item` 仍是上一次的 A，只比 ready 会拿 A 的旧判断
///     去跳过 B 的挂载，截到的就是张正在换台的画面。
///   - 渲染器最近一次 ready 的条目也是它（渲染器还没报过 ready 就不能算挂好）。
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
    all_match && crate::content_server::ready_item(app).as_deref() == Some(item_id)
}

/// 截图自检：可选先应用 → 等渲染就绪 → 实拍 → 可选存 preview.png
async fn screenshot(app: &AppHandle, args: &Value) -> Result<Value, String> {
    let item_id = req_str(args, "itemId")?;
    let apply = args.get("apply").and_then(|v| v.as_bool()).unwrap_or(true);
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
    let skip_apply = apply && already_ready(app, &item_id);
    // 先读 ready 时间戳再应用：否则可能立刻读到上一张壁纸的旧时间戳。
    // 跳过应用时 t0 置 0 —— 当前时间戳本就属于这张壁纸，不该再等它「变得更新」
    let t0 = if skip_apply {
        0
    } else {
        crate::system_wallpaper::ready_stamp(app)
    };
    if apply && !skip_apply {
        let app2 = app.clone();
        let id = item_id.clone();
        blocking(move || crate::wallpaper::apply_item(app2, id)).await?;
    }
    let png = crate::system_wallpaper::capture_wallpaper_png(app, t0, settle, timeout).await?;

    let mut saved_to = Value::Null;
    if save_preview {
        let project = match opt_str(args, "project") {
            Some(p) => Some(p),
            None => workspace::find_project_by_item(app, &item_id).ok().flatten(),
        };
        if let Some(project) = project {
            let app2 = app.clone();
            let bytes = png.clone();
            let path = blocking(move || workspace::save_preview(&app2, &project, &bytes)).await?;
            saved_to = json!(path);
        }
    }

    Ok(json!({
        "itemId": item_id,
        "bytes": png.len(),
        "previewPath": saved_to,
        // 本次是否真的重新应用过（false = 复用了已经挂好的同一张壁纸）
        "applied": apply && !skip_apply,
        "_image": B64.encode(&png),
    }))
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

/// 截图结果里的 `_image`（base64）要转成 MCP 的 image 内容块，只取一次
pub fn extract_image(result: &Value) -> Option<(String, String)> {
    let b64 = result.get("_image").and_then(|v| v.as_str())?;
    Some((b64.to_string(), "image/png".to_string()))
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
            assert_eq!(schema["type"], json!("object"), "{name} 的 inputSchema 不是 object");
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
