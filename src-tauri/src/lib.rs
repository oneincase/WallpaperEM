//! WallpaperEM —— Tauri 应用入口
//! T0：骨架 + T0.5 壁纸引擎原型；T1：Steam 客户端（登录/工坊）

mod audio_capture;
#[cfg(target_os = "linux")]
mod blur;
mod commands;
mod content_server;
mod db;
mod download;
mod keychain;
mod library;
mod main_window;
mod misc;
mod now_playing;
mod props_window;
mod secure_store;
mod steam;
mod system_wallpaper;
mod util;
mod wallpaper;
mod we_props;
mod we_shim;
mod workshop;

use rusqlite::Connection;
use std::sync::{Arc, Mutex};
#[cfg(target_os = "macos")]
use tauri::menu::MenuItemKind;
use tauri::{
    menu::{CheckMenuItemBuilder, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

pub fn run() {
    tauri::Builder::default()
        // 单实例：二次启动聚焦主窗口（已被闲置释放则重建）
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
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    use tauri_plugin_global_shortcut::ShortcutState;
                    if event.state() == ShortcutState::Pressed {
                        // Shortcut 实现 Display（如 "CommandOrControl+Shift+P"），按末字符区分
                        let s = shortcut.to_string();
                        let last = s.chars().last().unwrap_or('?').to_ascii_lowercase();
                        tracing::info!("global shortcut pressed: {s}");
                        match last {
                            'p' => {
                                // ⌘⇧P：切换暂停/恢复
                                let paused = app
                                    .try_state::<wallpaper::WallpaperEngineState>()
                                    .map(|s| *s.paused.lock().unwrap())
                                    .unwrap_or(false);
                                if paused {
                                    let _ = wallpaper::resume_all(app.clone());
                                } else {
                                    let _ = wallpaper::pause_all(app.clone());
                                }
                            }
                            'n' => {
                                // ⌘⇧N：下一张（轮播）
                                if let Err(e) = wallpaper::next(app.clone()) {
                                    tracing::warn!("next failed: {e}");
                                }
                            }
                            _ => {}
                        }
                    }
                })
                .build(),
        )
        // ⌘H 自定义为「最小化主窗口」：macOS 默认的 Hide 走 NSApplication hide，
        // 会把桌面级壁纸窗口一起藏掉；对壁纸引擎来说「隐藏」的合理语义是
        // 主窗口最小化（setup 里已把应用菜单的 Hide 项替换成本项）
        .on_menu_event(|app, event| {
            if event.id.as_ref() == "minimize_main" {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.minimize();
                }
            }
        })
        .setup(|app| {
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
            // 应用菜单改造：⌘H 从「隐藏应用」换成「最小化主窗口」
            // （壁纸窗口另有 canHide=false 兜底，双保险）
            #[cfg(target_os = "macos")]
            {
                let menu = Menu::default(app.handle())?;
                if let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next() {
                    let items = app_menu.items()?;
                    // Menu::default 的 app 子菜单布局固定：
                    // [About, sep, Services, sep, Hide, HideOthers, sep, Quit]
                    if let Some(hide) = items.get(4) {
                        let _ = app_menu.remove(hide);
                    }
                    let minimize = MenuItem::with_id(
                        app.handle(),
                        "minimize_main",
                        "最小化主窗口",
                        true,
                        Some("Cmd+H"),
                    )?;
                    app_menu.insert(&minimize, 4usize)?;
                }
                app.set_menu(menu)?;
            }
            // 托盘失败不致命：Linux 无托盘协议的环境（GNOME 未装 AppIndicator 扩展、
            // 容器/无头会话）里 TrayIconBuilder::build 会报错，不能让整个 setup 崩掉 ——
            // 退化为「无托盘常驻」，主窗口与壁纸功能照常（研究文档 §3.2 的降级策略）
            if let Err(e) = build_tray(app.handle()) {
                tracing::warn!("tray init failed, running without tray: {e}");
            }
            register_shortcuts(app.handle())?;
            // 音频捕获状态须先于内容服务器（SSE 端点读取其共享频谱帧）
            audio_capture::init(app.handle())?;
            // 「正在播放」订阅同理：/now-playing SSE 读它的共享快照。
            // 常驻子进程，起不来只是媒体集成不可用，不影响其他功能
            now_playing::start(app.handle());
            content_server::init(app.handle()).map_err(|e| e.to_string())?;
            // 开启音频可视化时先启动系统音频捕获，壁纸引擎（wallpaper::init）会
            // 有界等待其就绪后再创建壁纸窗口：保证壁纸页加载时注入服务已可用
            audio_capture::start_if_enabled(app.handle());
            wallpaper::init(app.handle())?;
            wallpaper::start_playlist_rotation(app.handle());
            download::init(app.handle()).map_err(|e| e.to_string())?;
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
                    match wallpaper::apply_item(app2.clone(), item_id.clone()) {
                        Ok(_) => tracing::info!("AUTO APPLY OK: {item_id}"),
                        Err(e) => tracing::error!("AUTO APPLY FAILED: {e}"),
                    }
                });
            }
            // 诊断：主窗口可见性
            if let Some(w) = app.get_webview_window("main") {
                let visible = w.is_visible().unwrap_or(false);
                tracing::info!("main window visible={visible}");
                // 关闭主窗口 = 隐藏（壁纸继续运行；点 Dock 图标重新显示）
                main_window::register_close_to_hide(&w);
            }
            // 主窗口闲置释放：隐藏/最小化持续 main_window::RELEASE_AFTER 后销毁窗口，
            // 回收其 WebContent 进程（壁纸窗口不受影响）；唤起时按需重建
            main_window::start(app.handle());
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
            commands::app_info,
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
            wallpaper::active_items,
            wallpaper::pause_all,
            wallpaper::resume_all,
            wallpaper::set_volume,
            wallpaper::set_fit,
            wallpaper::set_render_dpr,
            wallpaper::set_language,
            wallpaper::set_scene_fps,
            wallpaper::item_play_config_get,
            wallpaper::item_play_config_set,
            wallpaper::interactive_set,
            wallpaper::next,
            wallpaper::playlist_list,
            wallpaper::playlist_create,
            wallpaper::playlist_delete,
            wallpaper::playlist_apply,
            audio_capture::audio_processing_set,
            audio_capture::audio_processing_status,
            content_server::content_server_status,
            workshop::workshop_search,
            workshop::workshop_random,
            workshop::workshop_item,
            download::download_tool_status,
            download::steamcmd_install_tool,
            download::steamcmd_uninstall_tool,
            download::download_credentials_set,
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
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| match event {
            // macOS：点击 Dock 图标 / Finder 重开应用 → 显示主窗口
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => main_window::ensure_main_window(_app),
            // 最后一个窗口被销毁 ≠ 退出：本应用是常驻托盘的壁纸引擎，主窗口会被
            // 闲置释放、壁纸窗口可能被 stop() 清空，此前放任默认行为会直接退出进程
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
                crate::now_playing::stop();
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

/// 托盘：显示主窗口 / 壁纸设置 / 暂停播放 / 全局快速设置（显示模式·清晰度·帧率上限）/ 退出
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let item_props = MenuItem::with_id(app, "item_props", "壁纸设置", true, None::<&str>)?;

    // 全局快速设置：与设置页同一份持久化（settings 表），初始勾选读当前值
    let cur_fit = tray_read_setting(app, "wallpaper_fit", "cover");
    let cur_dpr = tray_read_setting(app, "wallpaper_render_dpr", "1");
    let cur_fps = tray_read_setting(app, "wallpaper_scene_fps", "60");
    let cur_auto_pause = tray_read_setting(app, "wallpaper_auto_pause", "false");
    // 自动暂停：切到非桌面应用自动暂停、回桌面自动播放（默认关，见 macos.rs 观察者）
    let auto_pause_item = CheckMenuItemBuilder::with_id("auto_pause", "自动暂停")
        .checked(cur_auto_pause == "true" || cur_auto_pause == "1")
        .build(app)?;

    let mk_check = |id: &str, text: &str, checked: bool| {
        CheckMenuItemBuilder::with_id(id, text)
            .checked(checked)
            .build(app)
    };
    // 单选语义：CheckMenuItem 自身不做互斥，点击后在事件里手动同步整组勾选
    let fit_items = [
        mk_check("fit_cover", "裁剪", cur_fit == "cover")?,
        mk_check("fit_contain", "缩放", cur_fit == "contain")?,
        mk_check("fit_stretch", "拉伸", cur_fit == "stretch")?,
    ];
    let dpr_items = [
        mk_check("dpr_0.8", "省电", cur_dpr == "0.8")?,
        mk_check("dpr_1", "标准", cur_dpr == "1")?,
        mk_check("dpr_2", "高清", cur_dpr == "2")?,
    ];
    let fps_items = [
        mk_check("fps_30", "30 FPS", cur_fps == "30")?,
        mk_check("fps_60", "60 FPS", cur_fps == "60")?,
        mk_check("fps_120", "120 FPS", cur_fps == "120")?,
    ];
    let fit_menu = Submenu::with_items(
        app,
        "显示模式",
        true,
        &[&fit_items[0], &fit_items[1], &fit_items[2]],
    )?;
    let dpr_menu = Submenu::with_items(
        app,
        "清晰度",
        true,
        &[&dpr_items[0], &dpr_items[1], &dpr_items[2]],
    )?;
    let fps_menu = Submenu::with_items(
        app,
        "帧率上限",
        true,
        &[&fps_items[0], &fps_items[1], &fps_items[2]],
    )?;

    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &item_props,
            &auto_pause_item,
            &sep1,
            &fit_menu,
            &dpr_menu,
            &fps_menu,
            &sep2,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .tooltip("WallpaperEM")
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| {
            let id = event.id.as_ref();
            match id {
                "show" => main_window::ensure_main_window(app),
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
                    let _ = auto_pause_item.set_checked(next);
                    // 关闭时若正挂在自动暂停上，立即恢复播放（标志一清，
                    // 回桌面也不会再代劳恢复了）
                    if !next {
                        if let Some(st) = app.try_state::<wallpaper::WallpaperEngineState>() {
                            let was_auto = {
                                let mut g = st.auto_paused.lock().unwrap();
                                std::mem::replace(&mut *g, false)
                            };
                            if was_auto {
                                let _ = wallpaper::resume_all(app.clone());
                            }
                        }
                    }
                }
                "quit" => app.exit(0),
                _ => {
                    // 全局快速设置：复用设置页的三个 command 实现（持久化 + 实时下发）
                    if let Some(fit) = id.strip_prefix("fit_") {
                        let _ = wallpaper::set_fit(app.clone(), fit.to_string());
                        for item in &fit_items {
                            let _ = item.set_checked(item.id() == &event.id);
                        }
                    } else if let Some(dpr) = id.strip_prefix("dpr_") {
                        if let Ok(v) = dpr.parse::<f32>() {
                            let _ = wallpaper::set_render_dpr(app.clone(), v);
                            for item in &dpr_items {
                                let _ = item.set_checked(item.id() == &event.id);
                            }
                        }
                    } else if let Some(fps) = id.strip_prefix("fps_") {
                        if let Ok(v) = fps.parse::<u32>() {
                            let _ = wallpaper::set_scene_fps(app.clone(), v);
                            for item in &fps_items {
                                let _ = item.set_checked(item.id() == &event.id);
                            }
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
    Ok(())
}

/// 读 settings 表（托盘菜单初始勾选/事件处理共用；键不存在返回 default）
fn tray_read_setting(app: &AppHandle, key: &str, default: &str) -> String {
    app.try_state::<Arc<Mutex<Connection>>>()
        .and_then(|db| db.lock().ok().and_then(|c| db::get_setting(&c, key)))
        .unwrap_or_else(|| default.to_string())
}

/// 全局快捷键（骨架）：⌘⇧P 暂停/恢复、⌘⇧N 下一张（轮播占位）
fn register_shortcuts(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    for s in ["cmd+shift+p", "cmd+shift+n"] {
        match app.global_shortcut().register(s) {
            Ok(_) => tracing::info!("shortcut registered: {s}"),
            Err(e) => tracing::warn!("shortcut register failed {s}: {e}"),
        }
    }
    Ok(())
}

/// 主窗口背景材质：macOS 侧栏 vibrancy（window-vibrancy，NSVisualEffectView）；
/// Linux 合成器模糊（KDE KWin，见 blur.rs）。pub(crate)：main_window 重建窗口后需再次应用。
pub(crate) fn apply_vibrancy(app: &AppHandle) -> tauri::Result<()> {
    #[cfg(target_os = "linux")]
    blur::apply(app);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = app;
    #[cfg(target_os = "macos")]
    {
        if let Some(w) = app.get_webview_window("main") {
            match window_vibrancy::apply_vibrancy(
                &w,
                window_vibrancy::NSVisualEffectMaterial::Sidebar,
                None,
                Some(16.0),
            ) {
                Ok(_) => tracing::info!("vibrancy applied to main window"),
                Err(e) => tracing::warn!("vibrancy apply failed: {e}"),
            }
        }
    }
    Ok(())
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
