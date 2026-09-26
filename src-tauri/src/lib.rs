//! WallpaperEM —— Tauri 应用入口
//! T0：骨架 + T0.5 壁纸引擎原型；T1：Steam 客户端（登录/工坊）

mod audio_capture;
#[cfg(target_os = "linux")]
mod blur;
mod commands;
mod content_server;
mod cover_cache;
mod db;
mod download;
mod ffmpeg;
mod hotkeys;
mod i18n;
mod keychain;
mod library;
mod main_window;
mod mcp;
mod media_bridge;
mod mem_pressure;
mod mem_watch;
mod misc;
mod now_playing;
mod props_window;
mod secure_store;
mod steam;
mod subscriptions;
mod system_wallpaper;
mod update;
mod util;
mod wallpaper;
mod we_assets;
#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
mod workshop_upload;
mod workspace;
mod we_props;
mod we_shim;
mod workshop;

use rusqlite::Connection;
use std::sync::{Arc, Mutex};
use tauri::{
    menu::{CheckMenuItem, CheckMenuItemBuilder, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager,
};

pub fn run() {
    tauri::Builder::default()
        // 单实例：二次启动聚焦主窗口（已被内存压力回收则重建）
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            main_window::ensure_main_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_desktop_underlay::init())
        // 应用内更新（配置在 tauri.conf.json 的 plugins.updater；JS 侧未授权，仅走自定义命令）
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    use tauri_plugin_global_shortcut::ShortcutState;
                    if event.state() == ShortcutState::Pressed {
                        tracing::info!("global shortcut pressed: {shortcut}");
                        hotkeys::dispatch_by_shortcut(app, shortcut);
                    }
                })
                .build(),
        )
        // 菜单快捷键（⌘M/⌘H 等 macOS 系统惯例键走这里，见 hotkeys 模块注释）
        .on_menu_event(|app, event| {
            let id = event.id.as_ref();
            if !hotkeys::handle_menu_event(app, id) {
                tracing::debug!("unhandled menu event: {id}");
            }
        })
        .setup(|app| {
            // 原生文案（托盘菜单 / 应用菜单 / 窗口标题）的语言兜底值要先定下来，
            // 下面几步就会用到；前端的权威值随后由 app_set_locale 推过来。
            i18n::init_from_system();
            init_logging(app.handle())?;
            // panic 钩子：tokio 任务与主线程回调里的 panic 会被各自的
            // catch_unwind 吞掉或仅在 stderr 打印（dev 终端不可见），统一落盘
            // 到滚动日志，保证「软件无响应」类问题可追溯
            {
                let hook = std::panic::take_hook();
                std::panic::set_hook(Box::new(move |info| {
                    let loc = info
                        .location()
                        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                        .unwrap_or_default();
                    tracing::error!("PANIC {loc}: {info}");
                    hook(info);
                }));
            }
            db::init(app.handle())?;
            init_steam(app.handle())?;
            // 自愈：升级老版本写下的无条件 KeepAlive plist（否则用户退出会被
            // launchd 立即复活，表现为"退出软件后一直自己重新启动"）
            #[cfg(target_os = "macos")]
            commands::ensure_keepalive_startup(app.handle());
            // 应用菜单（macOS）与快捷键注册都归 hotkeys 模块：⌘M/⌘H 这类系统
            // 惯例键落菜单键位、其余组合落全局热键，绑定可由「快捷键」页录制改写。
            // 菜单裁剪策略（只留 app 子菜单 + Edit）也内含在 build_app_menu 里。
            hotkeys::init(app.handle())?;
            // 托盘失败不致命：Linux 无托盘协议的环境（GNOME 未装 AppIndicator 扩展、
            // 容器/无头会话）里 TrayIconBuilder::build 会报错，不能让整个 setup 崩掉 ——
            // 退化为「无托盘常驻」，主窗口与壁纸功能照常（研究文档 §3.2 的降级策略）
            // 托盘菜单的句柄要留着（语言切换时原地改文字，不重建菜单，见 TrayMenu）
            let tray = match build_tray(app.handle()) {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::warn!("tray init failed, running without tray: {e}");
                    None
                }
            };
            app.manage(NativeMenuState { tray });
            // 轮播状态文案（当前项 / 暂停·恢复）按启动时状态刷新一次
            update_tray_rotation(app.handle());
            // 抽帧组件（ffmpeg）的托管路径：抽帧入口拿不到 AppHandle，启动时缓存一份
            ffmpeg::init(app.handle());
            // 音频捕获状态须先于内容服务器（SSE 端点读取其共享频谱帧）
            audio_capture::init(app.handle())?;
            // 全平台媒体桥接（正在播放元数据 + 系统音频采集的统一引擎）：
            // /now-playing SSE 与 audio_capture 都从这一个 bridge 取数据。
            // 起不来只是媒体集成不可用，不影响其他功能
            media_bridge::start(app.handle());
            content_server::init(app.handle()).map_err(|e| e.to_string())?;
            // 视频封面后台补齐：不在列表路径里做（历史版本在列表里惰性抽帧，
            // 解不动的格式每次刷新重试一遍，直接把库页面冻死）
            app.manage(library::PosterFailState::default());
            library::spawn_poster_backfill(app.handle().clone());
            // 封面本地化队列：本地库列表发现「目录里没封面但有远端 URL」时入队，
            // 由后台 worker 取回写成 preview.jpg/preview.gif（列表路径只入队）
            app.manage(Arc::new(cover_cache::CoverCacheState::default()));
            // 创意工坊上传：任务表 + Steam 客户端懒初始化（首次上传时才连 Steam）
            #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
            app.manage(workshop_upload::UploadState::default());
            // 开启音频可视化时先启动系统音频捕获，壁纸引擎（wallpaper::init）会
            // 有界等待其就绪后再创建壁纸窗口：保证壁纸页加载时注入服务已可用
            audio_capture::start_if_enabled(app.handle());
            wallpaper::init(app.handle())?;
            wallpaper::start_playlist_rotation(app.handle());
            // MCP 服务（默认关闭；开启后只绑回环地址）。放在 DB/壁纸引擎之后：
            // 工具全都依赖它们，早启动只会让首个请求撞上未就绪状态
            mcp::init(app.handle()).map_err(|e| e.to_string())?;
            download::init(app.handle()).map_err(|e| e.to_string())?;
            // 深色玻璃设计：先把 AppKit 外观钉成 dark 再上材质 —— vibrancy 材质
            // 亮度跟随系统外观，系统浅色模式下材质发白，深色 tint 盖不住、白字
            // 对比度掉档。只影响本应用（材质 + 原生控件），不改系统设置
            #[cfg(target_os = "macos")]
            force_dark_appearance();
            apply_vibrancy(app.handle())?;
            // T1 验证钩子：WE_AUTO_WORKSHOP=1 时启动即搜索第一页并打日志
            if std::env::var("WE_AUTO_WORKSHOP").as_deref() == Ok("1") {
                let svc = app.state::<Arc<workshop::WorkshopService>>();
                let svc = svc.inner().clone();
                tauri::async_runtime::spawn(async move {
                    match svc.search(Default::default()).await {
                        Ok(r) => tracing::info!(
                            "AUTO WORKSHOP: {} items, total={}, hasMore={}, first={:?}",
                            r.items.len(),
                            r.total,
                            r.has_more,
                            r.items.first().map(|i| i.title.clone())
                        ),
                        Err(e) => tracing::error!("AUTO WORKSHOP failed: {e}"),
                    }
                });
            }
            // T3 验证钩子：WE_AUTO_APPLY_ITEM=<itemId> 时从 Web 版数据导入并应用到桌面
            if let Ok(item_id) = std::env::var("WE_AUTO_APPLY_ITEM") {
                let app2 = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let web_data = std::env::var("WE_WEB_DATA").unwrap_or_else(|_| {
                        "/Users/oneincase/Documents/workspace/wallpaper engine/apps/server/data"
                            .into()
                    });
                    let _ = library::library_import_from_web(app2.clone(), web_data);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    match wallpaper::apply_item(app2.clone(), item_id.clone(), None) {
                        Ok(_) => tracing::info!("AUTO APPLY OK: {item_id}"),
                        Err(e) => tracing::error!("AUTO APPLY FAILED: {e}"),
                    }
                });
            }
            // 诊断：主窗口可见性
            if let Some(w) = app.get_webview_window("main") {
                let visible = w.is_visible().unwrap_or(false);
                tracing::info!("main window visible={visible}");
                // 关闭主窗口 = 立即销毁并结束其 WebContent 进程（释放内存，
                // 桌面只留壁纸渲染进程）；点 Dock 图标/托盘按需重建
                main_window::register_close_to_release(&w);
            }
            // 主窗口按需回收：只在系统内存压力下销毁隐藏中的窗口，回收它的
            // WebContent 进程（壁纸窗口不受影响）；唤起时按需重建
            main_window::start(app.handle());
            // 内存观测：每 60s 记一行「本进程 + WebKit 子进程」占用（仅 macOS 有读数），
            // 用来判断内存到底涨在我们自己的进程还是某个 WebContent 上
            mem_watch::start_periodic_report();
            // Linux 启动体检：GStreamer 插件缺失 = 视频壁纸黑屏/无声，
            // 缺啥把对应发行版的安装命令打进日志（首次运行 gst-inspect
            // 要建注册表缓存，可能耗时一秒级，放阻塞线程）
            #[cfg(target_os = "linux")]
            tauri::async_runtime::spawn_blocking(check_gstreamer_elements);
            tracing::info!("app setup complete");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            hotkeys::hotkeys_list,
            hotkeys::hotkeys_set,
            hotkeys::hotkeys_reset,
            hotkeys::hotkeys_record_begin,
            hotkeys::hotkeys_record_end,
            #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
            workshop_upload::workshop_upload_start,
            #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
            workshop_upload::workshop_upload_status,
            #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
            workshop_upload::workshop_web_upload_prepare,
            i18n::app_set_locale,
            commands::app_info,
            commands::app_pick_folder,
            commands::db_status,
            commands::settings_get,
            commands::settings_set,
            commands::autostart_status,
            commands::autostart_set,
            wallpaper::apply,
            wallpaper::apply_item,
            wallpaper::library_preview,
            wallpaper::stop,
            wallpaper::list_sessions,
            wallpaper::displays_list,
            wallpaper::display_binding_set,
            wallpaper::active_items,
            wallpaper::ui_backdrop,
            wallpaper::pause_all,
            wallpaper::resume_all,
            wallpaper::set_volume,
            wallpaper::set_fit,
            wallpaper::set_render_dpr,
            wallpaper::set_language,
            wallpaper::set_scene_fps,
            wallpaper::set_video_tex_scale,
            wallpaper::set_filter,
            wallpaper::set_reveal,
            wallpaper::set_aa,
            wallpaper::set_particles,
            wallpaper::set_post,
            wallpaper::set_resources,
            wallpaper::set_resources_normal,
            wallpaper::set_quality_preset,
            wallpaper::item_play_config_get,
            wallpaper::item_play_config_set,
            wallpaper::interactive_set,
            wallpaper::local_assets_set,
            wallpaper::we_assets_dir_set,
            wallpaper::local_assets_status,
            wallpaper::next,
            wallpaper::prev,
            wallpaper::rotation_set,
            wallpaper::playlist_list,
            wallpaper::playlist_get,
            wallpaper::playlist_create,
            wallpaper::playlist_update,
            wallpaper::playlist_delete,
            wallpaper::playlist_apply,
            wallpaper::playlist_stop,
            wallpaper::playlist_status,
            audio_capture::audio_processing_set,
            audio_capture::audio_processing_status,
            content_server::content_server_status,
            subscriptions::subscriptions_status,
            subscriptions::account_web_login_start,
            subscriptions::subscriptions_page,
            subscriptions::subscriptions_submit_code,
            subscriptions::subscriptions_qr_begin,
            subscriptions::subscriptions_qr_poll,
            subscriptions::subscriptions_logout,
            workshop::workshop_search,
            workshop::workshop_random,
            workshop::workshop_item,
            workshop::steam_author_summary,
            workshop::library_item_author,
            download::download_tool_status,
            ffmpeg::ffmpeg_status,
            ffmpeg::ffmpeg_install,
            ffmpeg::ffmpeg_uninstall,
            download::steamcmd_install_tool,
            download::steamcmd_uninstall_tool,
            download::download_credentials_set,
            download::download_verify_login,
            download::download_credentials_status,
            download::download_enqueue,
            download::download_list,
            download::download_cancel,
            download::download_retry,
            download::download_submit_guard,
            download::download_credentials_clear,
            download::download_remove,
            download::download_clear_finished,
            library::library_list,
            library::library_delete,
            library::library_prune,
            library::library_open_folder,
            library::library_import_from_web,
            library::library_import_custom,
            library::library_import_custom_pick,
            library::library_import_folder_pick,
            library::library_pick_folders,
            library::library_link_folders,
            library::library_import_custom_batch,
            library::item_props,
            library::item_title,
            library::set_item_props,
            library::reset_item_props,
            library::set_item_prop_file,
            misc::favorites_list,
            misc::favorite_add,
            misc::favorite_remove,
            misc::favorite_status,
            misc::network_probe,
            misc::diagnostics_export,
            misc::cache_stats,
            misc::cache_clear,
            update::app_update_check,
            update::app_update_download_install,
            update::app_update_restart,
            mcp::mcp_status,
            mcp::mcp_set_enabled,
            mcp::mcp_set_net_mode,
            mcp::mcp_set_port,
            mcp::mcp_rotate_token,
            mcp::mcp_config_snippet,
            mcp::shares::share_list_cmd,
            mcp::shares::share_create_cmd,
            mcp::shares::share_remove_cmd,
            mcp::shares::share_set_enabled_cmd,
            mcp::shares::share_enabled_status_cmd,
            mcp::shares::share_set_service_enabled_cmd,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| match event {
            // macOS：点击 Dock 图标 / Finder 重开应用 → 显示主窗口
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => main_window::ensure_main_window(_app),
            // 最后一个窗口被销毁 ≠ 退出：本应用是常驻托盘的壁纸引擎，主窗口会在内存
            // 压力下被回收、壁纸窗口可能被 stop() 清空，此前放任默认行为会直接退出进程
            // （睡眠时显示器列表异常触发的窗口清理即由此整进程退出）。
            // code = None 表示「窗口全关」而非主动退出；托盘「退出」走 app.exit(0)
            // （code = Some(0)），不受此拦截影响。
            tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } => {
                api.prevent_exit();
                tracing::info!("all windows destroyed; keep running in tray");
            }
            tauri::RunEvent::Exit => {
                crate::media_bridge::stop();
            }
            _ => {}
        });
}

/// 初始化 Steam 客户端与工坊服务（代理从设置读取）
fn init_steam(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let (proxy, follow_system_proxy): (Option<String>, bool) = {
        let db = app.state::<Arc<Mutex<Connection>>>();
        let conn = db.lock().map_err(|e| e.to_string())?;
        let proxy = db::get_setting(&conn, "steam_proxy");
        let follow = db::get_setting(&conn, "follow_system_proxy")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(true);
        (proxy, follow)
    };
    let client = steam::SteamClient::new(proxy, follow_system_proxy)?;
    app.manage(client.clone());
    app.manage(subscriptions::SubscriptionsState(std::sync::Mutex::new(None)));
    let db = app.state::<Arc<Mutex<Connection>>>();
    app.manage(Arc::new(workshop::WorkshopService::new(
        client,
        db.inner().clone(),
    )));
    Ok(())
}

/// 日志：stdout + 滚动文件（默认 ~/Library/Logs/<id>，写不了则回退到 app_data/logs，最后兜底 stdout；绝不因日志崩溃）
fn init_logging(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let default_dir = app.path().app_log_dir().unwrap_or_default();
    let fallback_dir = app
        .path()
        .app_data_dir()
        .map(|d| d.join("logs"))
        .unwrap_or_default();

    /// 尝试构建滚动日志 appender；目录不可写（如 macOS 对 ~/Library/Logs 加 ACL）时返回 None。
    fn try_appender(dir: &std::path::Path) -> Option<tracing_appender::non_blocking::NonBlocking> {
        let _ = std::fs::create_dir_all(dir);
        match tracing_appender::rolling::RollingFileAppender::builder()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("app")
            .build(dir)
        {
            Ok(a) => {
                let (writer, guard) = tracing_appender::non_blocking(a);
                // guard 需存活整个进程生命周期
                std::mem::forget(guard);
                Some(writer)
            }
            Err(e) => {
                eprintln!("[log] rolling appender 失败于 {}: {e}", dir.display());
                None
            }
        }
    }

    // 保证一定有一个可用的 writer（兜底用 stdout，避免日志造成 panic）
    let file_writer = try_appender(&default_dir)
        .or_else(|| try_appender(&fallback_dir))
        .unwrap_or_else(|| {
            let (writer, guard) = tracing_appender::non_blocking(std::io::stdout());
            std::mem::forget(guard);
            writer
        });

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,we_wallpaper=debug,tauri=warn,wry=warn".into());
    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(file_writer)
                .with_ansi(false),
        )
        .init();
    tracing::info!("logging ready: {}", default_dir.display());
    Ok(())
}

/// 画质档位表：(id 后缀, 中文标签)。低/中/高与设置页 QUALITY_PRESETS、
/// wallpaper::PRESET_* 同一套预设；「自定义」只是反推出来的状态（同设置页），
/// 点它不做事。档位判定按六个画质参数的当前值逐字段比对。
const QUALITY_ITEMS: &[(&str, &str)] = &[
    ("low", "低"),
    ("medium", "中"),
    ("high", "高"),
    ("custom", "自定义"),
];

/// 托盘菜单项的句柄集合。留着只为一件事：**切换语言时原地改文字**。
/// 不重建菜单 —— 重建会让 on_menu_event 闭包里捕获的勾选句柄指向已脱离菜单的
/// 旧对象，之后点「清晰度」就再没有勾选反馈了。
pub struct TrayMenu {
    show: MenuItem<tauri::Wry>,
    item_props: MenuItem<tauri::Wry>,
    auto_pause: CheckMenuItem<tauri::Wry>,
    rot_menu: Submenu<tauri::Wry>,
    rot_status: MenuItem<tauri::Wry>,
    rot_prev: MenuItem<tauri::Wry>,
    rot_next: MenuItem<tauri::Wry>,
    rot_pause: MenuItem<tauri::Wry>,
    quality_menu: Submenu<tauri::Wry>,
    quality_items: Vec<CheckMenuItem<tauri::Wry>>,
    filter_menu: Submenu<tauri::Wry>,
    filter_items: Vec<CheckMenuItem<tauri::Wry>>,
    reveal_menu: Submenu<tauri::Wry>,
    reveal_items: Vec<CheckMenuItem<tauri::Wry>>,
    quit: MenuItem<tauri::Wry>,
}

impl TrayMenu {
    fn retranslate(&self, app: &AppHandle) -> tauri::Result<()> {
        self.show.set_text(i18n::tr("显示主窗口"))?;
        self.item_props.set_text(i18n::tr("壁纸设置"))?;
        self.auto_pause.set_text(i18n::tr("自动暂停"))?;
        self.quit.set_text(i18n::tr("退出"))?;
        self.rot_menu.set_text(i18n::tr("轮播"))?;
        self.rot_prev.set_text(i18n::tr("上一张"))?;
        self.rot_next.set_text(i18n::tr("下一张"))?;
        self.quality_menu.set_text(i18n::tr("画质档位"))?;
        self.filter_menu.set_text(i18n::tr("滤镜效果"))?;
        self.reveal_menu.set_text(i18n::tr("切换效果"))?;
        for (item, (_, label)) in self.quality_items.iter().zip(QUALITY_ITEMS) {
            item.set_text(i18n::tr(label))?;
        }
        for (item, (_, label)) in self.filter_items.iter().zip(wallpaper::WALLPAPER_FILTERS) {
            item.set_text(i18n::tr(label))?;
        }
        for (item, (_, label)) in self.reveal_items.iter().zip(wallpaper::REVEAL_FX) {
            item.set_text(i18n::tr(label))?;
        }
        // 轮播状态文案是动态的（当前项 / 暂停·恢复），按当前状态重写
        self.refresh_rotation(app)?;
        Ok(())
    }

    /// 轮播状态 → 托盘文案。切换/激活/暂停后由 [`update_tray_rotation`] 调用。
    fn refresh_rotation(&self, app: &AppHandle) -> tauri::Result<()> {
        let st = wallpaper::playlist_status(app.clone())
            .unwrap_or_else(|_| serde_json::json!({ "active": false, "paused": false }));
        let paused = st.get("paused").and_then(|v| v.as_bool()).unwrap_or(false);
        let status = if st.get("active").and_then(|v| v.as_bool()).unwrap_or(false) {
            let name = st.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let index = st.get("index").and_then(|v| v.as_i64()).unwrap_or(0) + 1;
            let total = st.get("total").and_then(|v| v.as_i64()).unwrap_or(0);
            format!("{}：{} {}/{}", i18n::tr("轮播"), name, index, total)
        } else {
            i18n::tr("轮播：未启用").to_string()
        };
        self.rot_status.set_text(status)?;
        self.rot_pause
            .set_text(i18n::tr(if paused { "恢复轮播" } else { "暂停轮播" }))?;
        Ok(())
    }

    /// 某项共享设置改了（托盘自身点击 / 设置页 / MCP 任一入口）：把勾选同步到当前值。
    /// 单选语义 —— 档位表里没有匹配项时整组取消勾选（值非法或来自旧版本）。
    fn apply_setting(&self, key: &str, value: &str) {
        fn sync(items: &[CheckMenuItem<tauri::Wry>], table: &[(&str, &str)], value: &str) {
            for (item, (id, _)) in items.iter().zip(table) {
                let _ = item.set_checked(*id == value);
            }
        }
        match key {
            "wallpaper_auto_pause" => {
                let _ = self.auto_pause.set_checked(value == "true" || value == "1");
            }
            "wallpaper_filter" => sync(&self.filter_items, wallpaper::WALLPAPER_FILTERS, value),
            "wallpaper_reveal" => sync(&self.reveal_items, wallpaper::REVEAL_FX, value),
            // 画质六参数的勾选不在这里跟：档位是按整组值反推的，
            // 见 notify_setting_changed 的 apply_quality_preset 调用
            _ => {}
        }
    }

    /// 画质档位勾选（低/中/高/自定义单选）。`preset` 为 QUALITY_ITEMS 的 id。
    fn apply_quality_preset(&self, preset: &str) {
        for (item, (id, _)) in self.quality_items.iter().zip(QUALITY_ITEMS) {
            let _ = item.set_checked(*id == preset);
        }
    }
}

/// 共享设置（托盘快速设置 ↔ 设置页 ↔ MCP）写入后的统一通知：托盘勾选就地同步 +
/// 广播 `settings-changed`，让各窗口的控件（设置页下拉/开关、轮播条暂停钮）跟上。
/// 值以持久化后的字符串为准。**调用方须在 DB 锁外调用**（内部会拿锁反查画质参数
/// 反推档位勾选，Mutex 不可重入）。
pub(crate) fn notify_setting_changed(app: &AppHandle, key: &str, value: &str) {
    const QUALITY_KEYS: [&str; 6] = [
        "wallpaper_render_dpr",
        "wallpaper_scene_fps",
        "wallpaper_particles",
        "wallpaper_post",
        "wallpaper_resources",
        "wallpaper_resources_normal",
    ];
    if let Some(state) = app.try_state::<NativeMenuState>() {
        if let Some(tray) = &state.tray {
            tray.apply_setting(key, value);
            // 画质档位是按六个参数整组反推的：任一参数变了都重算一次勾选
            if QUALITY_KEYS.contains(&key) {
                if let Some(db) = app.try_state::<Arc<Mutex<Connection>>>() {
                    if let Ok(conn) = db.lock() {
                        tray.apply_quality_preset(wallpaper::derive_quality_preset_id(&conn));
                    }
                }
            }
        }
    }
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({ "key": key, "value": value }),
    );
}

/// 轮播状态变化（切换 / 激活 / 暂停）后刷新托盘「轮播」子菜单文案
pub(crate) fn update_tray_rotation(app: &AppHandle) {
    if let Some(state) = app.try_state::<NativeMenuState>() {
        if let Some(tray) = &state.tray {
            if let Err(e) = tray.refresh_rotation(app) {
                tracing::warn!("refresh tray rotation failed: {e}");
            }
        }
    }
}

/// 会被语言影响的原生菜单/标题。语言切换时由 retranslate_native_ui 整份重写
pub struct NativeMenuState {
    /// 托盘不可用的环境（Linux 无 AppIndicator 等）为 None，见 build_tray 的降级说明
    tray: Option<TrayMenu>,
}

/// 语言变了：把**原生绘制**的文案重写一遍（托盘菜单、macOS 应用菜单、独立窗口标题）。
/// 菜单只在打开的瞬间可见，所以效果是「下次打开就跟上」；这里不等用户操作，立即同步。
pub(crate) fn retranslate_native_ui(app: &AppHandle) {
    if let Some(state) = app.try_state::<NativeMenuState>() {
        if let Some(tray) = &state.tray {
            if let Err(e) = tray.retranslate(app) {
                tracing::warn!("retranslate tray failed: {e}");
            }
        }
    }
    // macOS 应用菜单里的快捷键项（⌘M/⌘H 等）跟着语言重写
    hotkeys::retranslate(app);
    // 独立「壁纸设置」窗口的原生标题由 Rust 侧绘制（label 形如 props-<itemId>）
    for (label, win) in app.webview_windows() {
        if label.starts_with("props-") {
            let _ = win.set_title(i18n::tr("壁纸设置"));
        }
    }
}

/// 勾选菜单项 → Submenu::with_items 要的借用切片（写成普通函数而非闭包，
/// 闭包推断不出「返回值借用入参」的生命周期）
fn as_refs(items: &[CheckMenuItem<tauri::Wry>]) -> Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> {
    items
        .iter()
        .map(|i| i as &dyn tauri::menu::IsMenuItem<tauri::Wry>)
        .collect()
}

/// 托盘：显示主窗口 / 壁纸设置 / 暂停播放 / 全局快速设置（显示模式·清晰度·帧率上限）/ 退出
fn build_tray(app: &AppHandle) -> tauri::Result<TrayMenu> {
    let show = MenuItem::with_id(app, "show", i18n::tr("显示主窗口"), true, None::<&str>)?;
    let item_props = MenuItem::with_id(app, "item_props", i18n::tr("壁纸设置"), true, None::<&str>)?;

    // 全局快速设置：与设置页同一份持久化（settings 表），初始勾选读当前值
    let cur_filter = tray_read_setting(app, "wallpaper_filter", wallpaper::DEFAULT_FILTER);
    let cur_auto_pause = tray_read_setting(app, "wallpaper_auto_pause", "false");
    // 自动暂停：切到非桌面应用自动暂停、回桌面自动播放（默认关，见 macos.rs 观察者）
    let auto_pause_item = CheckMenuItemBuilder::with_id("auto_pause", i18n::tr("自动暂停"))
        .checked(cur_auto_pause == "true" || cur_auto_pause == "1")
        .build(app)?;

    // 轮播（自动切换）：当前项 / 上一张 / 下一张 / 暂停自动切换。
    // 「暂停轮播」只停定时切换（wallpaper::rotation_set），不动壁纸渲染（那是全局「暂停播放」）。
    let rot_status =
        MenuItem::with_id(app, "rot_status", i18n::tr("轮播：未启用"), false, None::<&str>)?;
    let rot_prev = MenuItem::with_id(app, "rot_prev", i18n::tr("上一张"), true, None::<&str>)?;
    let rot_next = MenuItem::with_id(app, "rot_next", i18n::tr("下一张"), true, None::<&str>)?;
    let rot_pause = MenuItem::with_id(app, "rot_pause", i18n::tr("暂停轮播"), true, None::<&str>)?;
    let rot_menu = Submenu::with_items(
        app,
        i18n::tr("轮播"),
        true,
        &[
            &rot_status as &dyn tauri::menu::IsMenuItem<tauri::Wry>,
            &rot_prev,
            &rot_next,
            &rot_pause,
        ],
    )?;

    let mk_check = |id: &str, text: &str, checked: bool| {
        CheckMenuItemBuilder::with_id(id, text)
            .checked(checked)
            .build(app)
    };
    // 单选语义：CheckMenuItem 自身不做互斥，点击后在事件里手动同步整组勾选
    // 档位表是常量（QUALITY_ITEMS 等），构建与语言切换重写共用一份，避免两边走偏。
    // 勾选态按六参数反推（与设置页同规则），点低/中/高一键套用整个预设
    let cur_quality = {
        let db = app.try_state::<Arc<Mutex<Connection>>>();
        db.and_then(|db| db.lock().ok().map(|c| wallpaper::derive_quality_preset_id(&c)))
            .unwrap_or("custom")
    };
    let quality_items: Vec<CheckMenuItem<tauri::Wry>> = QUALITY_ITEMS
        .iter()
        .map(|(id, label)| mk_check(&format!("quality_{id}"), i18n::tr(label), cur_quality == *id))
        .collect::<tauri::Result<Vec<_>>>()?;
    let quality_menu = Submenu::with_items(app, i18n::tr("画质档位"), true, &as_refs(&quality_items))?;
    // 全局滤镜：id 白名单与渲染器的 CSS filter 表一一对应（见
    // wallpaper::WALLPAPER_FILTERS），这里只摆开关，表达式不经过原生侧。
    // 作用范围是桌面壁纸窗口，壁纸预览（主窗口里的 iframe）不受影响。
    let filter_items: Vec<tauri::menu::CheckMenuItem<tauri::Wry>> = wallpaper::WALLPAPER_FILTERS
        .iter()
        .map(|(id, label)| mk_check(&format!("filter_{id}"), i18n::tr(label), cur_filter == *id))
        .collect::<tauri::Result<Vec<_>>>()?;
    let filter_menu = Submenu::with_items(app, i18n::tr("滤镜效果"), true, &as_refs(&filter_items))?;
    // 无缝切换的过渡效果：同 filter 一套模式（白名单 id 只在渲染器里落成动画）。
    // 改动不热更活窗口 —— 下一次换壁纸时新窗按新效果显形
    let cur_reveal = tray_read_setting(app, "wallpaper_reveal", wallpaper::DEFAULT_REVEAL);
    let reveal_items: Vec<tauri::menu::CheckMenuItem<tauri::Wry>> = wallpaper::REVEAL_FX
        .iter()
        .map(|(id, label)| mk_check(&format!("reveal_{id}"), i18n::tr(label), cur_reveal == *id))
        .collect::<tauri::Result<Vec<_>>>()?;
    let reveal_menu = Submenu::with_items(app, i18n::tr("切换效果"), true, &as_refs(&reveal_items))?;

    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", i18n::tr("退出"), true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &item_props,
            &auto_pause_item,
            &rot_menu,
            &sep1,
            &quality_menu,
            &filter_menu,
            &reveal_menu,
            &sep2,
            &quit,
        ],
    )?;

    // 句柄副本留给 NativeMenuState（语言切换时改文字）；原件随 on_menu_event 闭包走
    let state = TrayMenu {
        show: show.clone(),
        item_props: item_props.clone(),
        auto_pause: auto_pause_item.clone(),
        rot_menu: rot_menu.clone(),
        rot_status: rot_status.clone(),
        rot_prev: rot_prev.clone(),
        rot_next: rot_next.clone(),
        rot_pause: rot_pause.clone(),
        quality_menu: quality_menu.clone(),
        quality_items: quality_items.clone(),
        filter_menu: filter_menu.clone(),
        reveal_menu: reveal_menu.clone(),
        filter_items: filter_items.clone(),
        reveal_items: reveal_items.clone(),
        quit: quit.clone(),
    };

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .tooltip("WallpaperEM")
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| {
            let id = event.id.as_ref();
            match id {
                "show" => main_window::ensure_main_window(app),
                "rot_prev" => {
                    if let Err(e) = wallpaper::prev(app.clone(), None) {
                        tracing::warn!("tray rot_prev: {e}");
                    }
                }
                "rot_next" => {
                    if let Err(e) = wallpaper::next(app.clone(), None) {
                        tracing::warn!("tray rot_next: {e}");
                    }
                }
                "rot_pause" => {
                    let paused = app
                        .try_state::<Arc<Mutex<Connection>>>()
                        .and_then(|db| {
                            db.lock()
                                .ok()
                                .and_then(|c| db::get_setting(&c, "playlist_rotation_paused"))
                        })
                        .map(|v| v == "true" || v == "1")
                        .unwrap_or(false);
                    if let Err(e) = wallpaper::rotation_set(app.clone(), !paused) {
                        tracing::warn!("tray rot_pause: {e}");
                    }
                }
                "item_props" => {
                    // 独立设置窗口：只带配置面板，不唤起主界面。
                    // 多显示器时取第一个已应用项（active_items 已过滤文件丢失）。
                    match wallpaper::active_items(app.clone()) {
                        Ok(ids) if !ids.is_empty() => {
                            if let Err(e) = props_window::open(app, &ids[0]) {
                                tracing::warn!("open props window for {} failed: {e}", ids[0]);
                            }
                        }
                        Ok(_) => {
                            // 没有已应用壁纸：没有可配置对象，唤出主窗口让用户先选一张
                            main_window::ensure_main_window(app);
                        }
                        Err(e) => tracing::warn!("tray item_props: {e}"),
                    }
                }
                "auto_pause" => {
                    let was = tray_read_setting(app, "wallpaper_auto_pause", "false");
                    let next = !(was == "true" || was == "1");
                    if let Some(db) = app.try_state::<Arc<Mutex<Connection>>>() {
                        if let Ok(conn) = db.lock() {
                            let _ = db::set_setting(
                                &conn,
                                "wallpaper_auto_pause",
                                if next { "true" } else { "false" },
                            );
                        }
                    }
                    // 托盘勾选 + 设置页同名开关都由这条统一通知跟上
                    notify_setting_changed(
                        app,
                        "wallpaper_auto_pause",
                        if next { "true" } else { "false" },
                    );
                    // 关闭时若正挂在自动暂停上，立即恢复播放（集合一清，
                    // 露出/回桌面也不会再代劳恢复了）
                    if !next {
                        if let Some(st) = app.try_state::<wallpaper::WallpaperEngineState>() {
                            let any_auto = !st.auto_paused.lock().unwrap().is_empty();
                            if any_auto {
                                let _ = wallpaper::resume_all(app.clone());
                            }
                        }
                    }
                }
                "quit" => app.exit(0),
                _ => {
                    // 全局快速设置：复用设置页的 command 实现（持久化 + 实时下发 +
                    // notify_setting_changed 统一同步勾选/设置页控件，这里不再手动勾选）
                    if let Some(quality) = id.strip_prefix("quality_") {
                        // 「自定义」是反推状态不是预设：点了不套用任何东西
                        if quality != "custom" {
                            if let Err(e) =
                                wallpaper::set_quality_preset(app.clone(), quality.to_string())
                            {
                                tracing::warn!("tray set quality preset failed: {e}");
                            }
                        }
                    } else if let Some(filter) = id.strip_prefix("filter_") {
                        // 白名单校验在 command 里；这里只负责调用，失败记日志
                        if let Err(e) = wallpaper::set_filter(app.clone(), filter.to_string()) {
                            tracing::warn!("tray set filter failed: {e}");
                        }
                    } else if let Some(reveal) = id.strip_prefix("reveal_") {
                        if let Err(e) = wallpaper::set_reveal(app.clone(), reveal.to_string()) {
                            tracing::warn!("tray set reveal fx failed: {e}");
                        }
                    }
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                main_window::ensure_main_window(tray.app_handle());
            }
        });
    // 菜单栏图标：专用模板图（纯黑剪影 + 透明底，图形顶格无内边距，icons/tray-44.png，
    // 由 scripts/make_tray_icon.py 从应用 logo 极简化生成）。macOS 上托盘图标必须显式设置，
    // 否则状态栏只显示一个「空位」。icon_as_template(true) 是原生状态栏做法：
    // 系统按菜单栏深浅自动着色——深色菜单栏呈白色、浅色呈黑色。
    // 编译期内嵌，不依赖 bundle 图标；Windows/Linux 不识别模板，沿用原彩色应用图标。
    #[cfg(target_os = "macos")]
    {
        builder = builder.icon(tauri::include_image!("icons/tray-44.png"));
        builder = builder.icon_as_template(true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(icon) = app.default_window_icon() {
            let icon = icon.clone();
            tracing::info!(
                "tray icon: default_window_icon {}x{}",
                icon.width(),
                icon.height()
            );
            builder = builder.icon(icon);
        } else {
            tracing::warn!("default_window_icon is None; creating tray without an icon");
        }
    }
    builder.build(app)?;
    tracing::info!("tray icon built");
    Ok(state)
}

/// 读 settings 表（托盘菜单初始勾选/事件处理共用；键不存在返回 default）
fn tray_read_setting(app: &AppHandle, key: &str, default: &str) -> String {
    app.try_state::<Arc<Mutex<Connection>>>()
        .and_then(|db| db.lock().ok().and_then(|c| db::get_setting(&c, key)))
        .unwrap_or_else(|| default.to_string())
}

/// 把应用外观钉成深色（macOS）：NSVisualEffectView 的材质亮度跟随外观，系统
/// 浅色模式下材质发白，深色玻璃 tint 盖不住、白字对比度掉档。setup 主线程调用；
/// 失败只丢观感（材质退回跟随系统），不影响功能。
#[cfg(target_os = "macos")]
fn force_dark_appearance() {
    use objc2_app_kit::{NSAppearance, NSApplication};
    unsafe {
        let Some(dark) = NSAppearance::appearanceNamed(objc2_app_kit::NSAppearanceNameDarkAqua)
        else {
            tracing::warn!("NSAppearance darkAqua unavailable; vibrancy follows system");
            return;
        };
        NSApplication::sharedApplication(objc2::MainThreadMarker::new_unchecked())
            .setAppearance(Some(&dark));
        tracing::info!("app appearance pinned to darkAqua");
    }
}

/// 主窗口背景材质：macOS 侧栏 vibrancy（window-vibrancy，NSVisualEffectView）；
/// Windows Acrylic（Win10 1809+，失败退回老 blur 通道）；
/// Linux 合成器模糊（KDE KWin，见 blur.rs）。pub(crate)：main_window 重建窗口后需再次应用。
pub(crate) fn apply_vibrancy(app: &AppHandle) -> tauri::Result<()> {
    #[cfg(target_os = "linux")]
    blur::apply(app);
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if let Some(w) = app.get_webview_window("main") {
        apply_backdrop(&w);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let _ = app;
    Ok(())
}

/// 给单个窗口套平台背景材质（主窗口与壁纸属性窗口共用）。
///
/// - macOS：侧栏 vibrancy（NSVisualEffectView）
/// - Windows：Acrylic（DWM 系统背景，Win10 1809+）；不可用时退回 Win7/10 的 blur
///   （Win11 22621 上 blur 有已知的拖动/缩放卡顿，只在 Acrylic 失败时才用）
///
/// 失败只记日志：材质是纯装饰，不影响功能。
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) fn apply_backdrop(window: &tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    match window_vibrancy::apply_vibrancy(
        window,
        window_vibrancy::NSVisualEffectMaterial::Sidebar,
        None,
        // 与渲染器 CSS 圆角对齐（App.tsx 根容器 rounded-[12px]）：borderless
        // 窗口没有系统圆角了，材质自己圆 12px，CSS 圆角外的四角才是透明桌面
        Some(12.0),
    ) {
        Ok(_) => tracing::info!("vibrancy applied to {}", window.label()),
        Err(e) => tracing::warn!("vibrancy apply failed for {}: {e}", window.label()),
    }
    #[cfg(target_os = "windows")]
    {
        // 深色半透明底：与 macOS 侧栏材质、KWin blur 的观感对齐
        let tint = Some((28u8, 28u8, 32u8, 180u8));
        match window_vibrancy::apply_acrylic(window, tint) {
            Ok(_) => tracing::info!("acrylic applied to {}", window.label()),
            Err(e) => {
                tracing::debug!(
                    "acrylic unavailable for {} ({e}); falling back to blur",
                    window.label()
                );
                if let Err(e2) = window_vibrancy::apply_blur(window, tint) {
                    tracing::warn!("blur apply failed for {}: {e2}", window.label());
                }
            }
        }
    }
}

/// Linux 启动体检：WebKitGTK 的音视频播放完全依赖系统 GStreamer 插件，
/// 缺插件时视频壁纸黑屏/无声，且只有 WebKitWebProcess 里一行模糊的
/// "GStreamer element xxx not found"，用户很难定位。
/// 启动时主动探一遍关键 element，缺啥直接给出安装命令。
#[cfg(target_os = "linux")]
fn check_gstreamer_elements() {
    use std::process::Command;

    let has = |name: &str| -> Option<bool> {
        Command::new("gst-inspect-1.0")
            .arg(name)
            .output()
            .map(|o| o.status.success())
            .ok()
    };

    // (element, Debian/Ubuntu 包名, 缺了会怎样)
    const REQUIRED: &[(&str, &str, &str)] = &[
        ("appsink", "gstreamer1.0-plugins-base", "视频无法渲染（壁纸黑屏）"),
        ("autoaudiosink", "gstreamer1.0-plugins-good", "无声"),
        ("playbin", "gstreamer1.0-plugins-base", "媒体无法播放"),
    ];

    let mut missing: Vec<String> = Vec::new();
    let mut inspect_unavailable = false;
    for (element, pkg, impact) in REQUIRED {
        match has(element) {
            Some(true) => {}
            Some(false) => missing.push(format!("{element}（{pkg}）→ {impact}")),
            None => {
                inspect_unavailable = true;
                break;
            }
        }
    }

    // H.264 是工坊视频壁纸的主流编码：avdec_h264(libav) 与 openh264dec(bad) 二选一
    if !inspect_unavailable {
        let h264_ok = has("avdec_h264").unwrap_or(false) || has("openh264dec").unwrap_or(false);
        if !h264_ok {
            missing.push("H.264 解码器（gstreamer1.0-libav）→ mp4 视频壁纸无法播放".into());
        }
    }

    if inspect_unavailable {
        tracing::warn!(
            "gst-inspect-1.0 不可用，无法检测 GStreamer 插件完整性；             若视频壁纸黑屏/无声，请按下方指引安装插件"
        );
    }
    if missing.is_empty() && !inspect_unavailable {
        tracing::info!("gstreamer check: 关键 element 齐全（appsink/autoaudiosink/h264）");
        return;
    }
    for m in &missing {
        tracing::warn!("gstreamer 缺失: {m}");
    }
    tracing::warn!(
        "GStreamer 插件安装指引 — Debian/Ubuntu: sudo apt install          gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad          gstreamer1.0-libav | Fedora: sudo dnf install gstreamer1-plugins-base          gstreamer1-plugins-good gstreamer1-plugins-bad-free gstreamer1-libav |          Arch: sudo pacman -S gst-plugins-base gst-plugins-good gst-plugins-bad gst-libav"
    );
}
