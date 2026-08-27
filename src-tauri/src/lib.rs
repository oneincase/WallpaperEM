//! WallpaperEM —— Tauri 应用入口
//! T0：骨架 + T0.5 壁纸引擎原型；T1：Steam 客户端（登录/工坊）

mod commands;
mod content_server;
mod db;
mod download;
mod keychain;
mod secure_store;
mod library;
mod misc;
mod steam;
mod util;
mod wallpaper;
mod workshop;

use std::sync::{Arc, Mutex};
use rusqlite::Connection;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

pub fn run() {
    tauri::Builder::default()
        // 单实例：二次启动聚焦主窗口
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init())
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
        .setup(|app| {
            init_logging(app.handle())?;
            db::init(app.handle())?;
            init_steam(app.handle())?;
            build_tray(app.handle())?;
            register_shortcuts(app.handle())?;
            content_server::init(app.handle()).map_err(|e| e.to_string())?;
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
                        "/Users/oneincase/Documents/workspace/wallpaper engine/apps/server/data".into()
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
                let w2 = w.clone();
                w.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w2.hide();
                    }
                });
            }
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
            wallpaper::interactive_set,
            wallpaper::next,
            wallpaper::playlist_list,
            wallpaper::playlist_create,
            wallpaper::playlist_delete,
            wallpaper::playlist_apply,
            content_server::content_server_status,
            workshop::workshop_search,
            workshop::workshop_random,
            workshop::workshop_item,
            download::download_tool_status,
            download::download_credentials_set,
            download::download_credentials_status,
            download::download_enqueue,
            download::download_list,
            download::download_cancel,
            download::download_retry,
            download::download_submit_guard,
            download::download_qr_login,
            download::download_qr_submit_guard,
            download::download_qr_cancel,
            download::download_credentials_clear,
            download::download_remove,
            download::download_clear_finished,
            library::library_list,
            library::library_delete,
            library::library_open_folder,
            library::library_import_from_web,
            misc::favorites_list,
            misc::favorite_add,
            misc::favorite_remove,
            misc::favorite_status,
            misc::network_probe,
            misc::diagnostics_export,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // macOS：点击 Dock 图标 / Finder 重开应用 → 显示主窗口
            if let tauri::RunEvent::Reopen { .. } = event {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
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
    app.manage(Arc::new(workshop::WorkshopService::new(client, db.inner().clone())));
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

/// 托盘：显示主窗口 / 暂停恢复 / 下一张 / 退出
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "暂停 / 恢复壁纸", true, None::<&str>)?;
    let next = MenuItem::with_id(app, "next", "下一张（轮播）", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &pause, &next, &quit])?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .tooltip("WallpaperEM")
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            "pause" => {
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
            "next" => {
                if let Err(e) = wallpaper::next(app.clone()) {
                    tracing::warn!("next failed: {e}");
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
        });
    // 菜单栏图标。macOS 上托盘图标必须显式设置，否则状态栏只显示一个「空位」。
    // 用 default_window_icon()（打包时从 bundle.icon 生成），设为非模板保持应用图标颜色。
    // default_window_icon() 始终为 Some（配置含 .png 图标）；万一为 None 也继续创建托盘，
    // 不加图标，避免在 setup 阶段因图标问题整体启动失败。
    if let Some(icon) = app.default_window_icon() {
        let icon = icon.clone();
        tracing::info!("tray icon: default_window_icon {}x{}", icon.width(), icon.height());
        builder = builder.icon(icon);
        builder = builder.icon_as_template(false);
    } else {
        tracing::warn!("default_window_icon is None; creating tray without an icon");
    }
    builder.build(app)?;
    tracing::info!("tray icon built");
    Ok(())
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

/// macOS 材质：主窗口侧栏 vibrancy（window-vibrancy，基于 NSVisualEffectView）
fn apply_vibrancy(app: &AppHandle) -> tauri::Result<()> {
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
