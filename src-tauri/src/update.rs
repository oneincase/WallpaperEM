//! 软件更新：官方 `tauri-plugin-updater`（校验签名 → 下载进度 → 平台静默安装 → 重启生效）。
//!
//! 演进：早期版本刻意不用官方 updater（要求维护签名密钥 + Release 挂 latest.json 与
//! `.sig`），走「查 GitHub API → 下载安装包 → 打开安装器」的自定义流程；但它做不到
//! 「下载完重启即新版」（macOS 还得手动拖 DMG）。现改为官方插件，代价是接管签名
//! 基础设施：密钥由 `pnpm tauri signer generate` 生成（私钥只存 `~/.tauri/` 与
//! GitHub Secrets，绝不入库），CI 构建时用 `TAURI_SIGNING_PRIVATE_KEY` 签名，
//! 各平台 workflow 往 Release 追加 `latest-{target}-{arch}.json` 清单（见
//! `scripts/gen-update-manifest.mjs`），端点模板在 tauri.conf.json 的 plugins.updater。
//!
//! 流程：`check()` 读清单（比对 semver，无 api.github.com 速率限制）→ `download()`
//! 校验 minisign 签名、节流发 `update:progress` → `install()` 平台原生安装
//! （Windows 装完由 NSIS passive 窗口接管并自动重启本体；macOS/Linux 需调
//! `app_update_restart`）。代理沿用应用自己的设置（`download_proxy` → `steam_proxy`
//! 回退、`follow_system_proxy`），透传给插件的 `UpdaterBuilder::proxy/no_proxy`。
//!
//! 已知限制（失败时前端引导「前往下载页」兜底）：deb/rpm 安装的应用不支持应用内
//! 升级（updater 只覆盖 AppImage）；Release 清单里的 notes 是发布时快照。

use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

/// GitHub 仓库（owner/repo）
const REPO: &str = "oneincase/WallpaperEM";

/// 当前版本。以 **Tauri 包信息**为准（来自 tauri.conf.json，与发版 tag / 安装包一致）；
/// Cargo.toml 的 version 只用于 crate 元数据，两者不一致时以用户看到的安装包版本为准。
pub fn current_version(app: &AppHandle) -> String {
    app.package_info().version.to_string()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    /// 当前版本
    pub current: String,
    /// 最新版本（清单里的 semver，无 v 前缀）
    pub latest: String,
    pub has_update: bool,
    /// 版本标题（如 "v1.2.0"，原 Release 标题位）
    pub name: String,
    /// 更新说明（Release 正文快照，前端按纯文本展示）
    pub notes: String,
    pub published_at: String,
    /// Release 页面地址（检查失败 / 无法应用内更新时的兜底入口）
    pub html_url: String,
}

/// 读代理设置（与下载链路同一套键）
fn proxy_settings(app: &AppHandle) -> (Option<String>, bool) {
    let Some(db) = app.try_state::<Arc<Mutex<Connection>>>() else {
        return (None, true);
    };
    let Ok(conn) = db.lock() else {
        return (None, true);
    };
    let proxy = crate::db::get_setting(&conn, "download_proxy")
        .or_else(|| crate::db::get_setting(&conn, "steam_proxy"))
        .filter(|p| !p.trim().is_empty());
    let follow = crate::db::get_setting(&conn, "follow_system_proxy")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true);
    (proxy, follow)
}

/// 用 tauri.conf.json 里的 updater 配置（pubkey/endpoints）构建 Updater，
/// 并透传应用内代理设置。
fn build_updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    let (proxy, follow) = proxy_settings(app);
    let mut builder = app.updater_builder();
    if let Some(p) = proxy {
        let url = url::Url::parse(&p).map_err(|e| format!("代理配置无效: {e}"))?;
        builder = builder.proxy(url);
    } else if !follow {
        builder = builder.no_proxy();
    }
    // 不设 timeout：插件的 timeout 覆盖整个请求（含响应体），大安装包会被误杀
    builder.build().map_err(|e| e.to_string())
}

/// `check()` 失败 → 给用户看的中文说明（前端会再冠「检查更新失败：」前缀）
fn check_err(e: tauri_plugin_updater::Error) -> String {
    use tauri_plugin_updater::Error;
    match e {
        // 端点 404：清单还没挂上（发版中）或 Release 尚未创建
        Error::ReleaseNotFound => "更新清单不存在（新版本可能正在发布中）".into(),
        // 清单里没有 {os}-{arch} 平台键
        Error::TargetsNotFound(_) => "当前平台没有对应的更新清单".into(),
        other => other.to_string(),
    }
}

/// 检查是否有新版本（读 Release 里的 latest-{target}-{arch}.json 清单）
#[tauri::command(rename = "app_update_check")]
pub async fn app_update_check(app: AppHandle) -> Result<UpdateInfo, String> {
    let current = current_version(&app);
    let html_url = format!("https://github.com/{REPO}/releases");
    let updater = build_updater(&app)?;
    let found = updater.check().await.map_err(check_err)?;

    Ok(match found {
        Some(u) => UpdateInfo {
            current,
            latest: u.version.clone(),
            has_update: true,
            name: format!("v{}", u.version),
            notes: u.body.unwrap_or_default(),
            // time::OffsetDateTime → RFC3339（JS Date 可直接 parse）
            published_at: u
                .date
                .and_then(|d| chrono::DateTime::from_timestamp(d.unix_timestamp(), d.nanosecond()))
                .map(|d| d.to_rfc3339())
                .unwrap_or_default(),
            html_url,
        },
        None => UpdateInfo {
            latest: current.clone(),
            current,
            has_update: false,
            name: String::new(),
            notes: String::new(),
            published_at: String::new(),
            html_url,
        },
    })
}

/// 下载新版本（边下边发 `update:progress`，校验签名）并原地安装。
/// 返回后：macOS/Linux 调 `app_update_restart` 生效；Windows 上 install 会直接
/// 退出本进程（NSIS 装完自动重启），此命令在 Windows 上不会返回。
#[tauri::command(rename = "app_update_download_install")]
pub async fn app_update_download_install(app: AppHandle) -> Result<(), String> {
    let updater = build_updater(&app)?;
    let update = updater
        .check()
        .await
        .map_err(check_err)?
        .ok_or_else(|| "没有可用的更新（可能已是最新版本）".to_string())?;

    let mut received: u64 = 0;
    let mut total: u64 = 0;
    let mut last_emit: u64 = 0;
    let bytes = update
        .download(
            |chunk, content_length| {
                received += chunk as u64;
                total = content_length.unwrap_or(0);
                // 每 ~256KB 或最后一包推一次，避免小水管下事件风暴
                if received - last_emit >= 256 * 1024 || (total > 0 && received >= total) {
                    last_emit = received;
                    let _ = app.emit(
                        "update:progress",
                        json!({ "received": received, "total": total }),
                    );
                }
            },
            || {},
        )
        .await
        .map_err(|e| format!("下载更新失败: {e}"))?;
    let _ = app.emit(
        "update:progress",
        json!({ "received": received, "total": total.max(received) }),
    );

    let _ = app.emit("update:phase", json!({ "phase": "installing" }));
    update.install(&bytes).map_err(|e| format!("安装更新失败: {e}"))?;
    Ok(())
}

/// 安装完成后重启应用（Windows 上装完即退出，走不到这里）
#[tauri::command(rename = "app_update_restart")]
pub fn app_update_restart(app: AppHandle) {
    app.restart();
}
