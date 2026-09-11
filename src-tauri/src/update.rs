//! 软件更新：检查 GitHub Releases、下载安装包并交给系统安装。
//!
//! 为什么不用 `tauri-plugin-updater`：官方 updater 要求发布方维护签名密钥、
//! 并在 release 里附带 `latest.json` 与签名文件；本项目直接在 GitHub 上以
//! `.dmg` / `.exe|.msi` / `.AppImage|.deb` 分发（见 `.github/workflows/`），
//! 所以走「查 latest release → 比对版本 → 下载对应平台安装包 → 打开安装器」
//! 这条不依赖签名基础设施的路径。
//!
//! 网络请求沿用应用自己的代理设置（`download_proxy` → `steam_proxy` 回退，
//! 与下载链路同一套），系统代理跟随 `follow_system_proxy`。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

/// GitHub 仓库（owner/repo）
const REPO: &str = "oneincase/WallpaperEM";
/// 单次请求超时：检查很轻，下载用同一 client 但超时只约束「连接与首字节」
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// 当前版本。以 **Tauri 包信息**为准（来自 tauri.conf.json，与发版 tag / 安装包一致）；
/// Cargo.toml 的 version 只用于 crate 元数据，两者不一致时以用户看到的安装包版本为准。
pub fn current_version(app: &AppHandle) -> String {
    app.package_info().version.to_string()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAsset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    /// 当前版本
    pub current: String,
    /// 最新版本（去掉 tag 前缀 v）
    pub latest: String,
    pub has_update: bool,
    /// Release 标题
    pub name: String,
    /// Release 说明（Markdown 原文，前端按纯文本展示）
    pub notes: String,
    pub published_at: String,
    /// Release 页面地址（拿不到平台安装包时的兜底入口）
    pub html_url: String,
    /// 当前平台匹配到的安装包；没有则为 None（仍可去 html_url 下载）
    pub asset: Option<UpdateAsset>,
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

fn build_client(app: &AppHandle) -> Result<reqwest::Client, String> {
    let (proxy, follow) = proxy_settings(app);
    let mut builder = reqwest::Client::builder()
        .user_agent(format!("WallpaperEM/{}", current_version(app)))
        .timeout(HTTP_TIMEOUT);    if let Some(p) = proxy {
        builder = builder
            .proxy(reqwest::Proxy::all(&p).map_err(|e| format!("代理配置无效: {e}"))?);
    } else if !follow {
        builder = builder.no_proxy();
    }
    builder.build().map_err(|e| e.to_string())
}

/// 版本号 → (major, minor, patch)；非数字段按 0，预发布后缀（-rc.1）不参与比较
fn ver_tuple(s: &str) -> (u64, u64, u64) {
    let core = s
        .trim()
        .trim_start_matches(|c| c == 'v' || c == 'V')
        .split(|c: char| c == '-' || c == '+')
        .next()
        .unwrap_or("");
    let mut it = core.split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}

fn is_newer(latest: &str, current: &str) -> bool {
    ver_tuple(latest) > ver_tuple(current)
}

/// 按平台优先级挑安装包
fn pick_asset<'a>(assets: &'a [UpdateAsset]) -> Option<&'a UpdateAsset> {
    // 顺序即优先级：Windows 的 NSIS(.exe) 比 MSI 更好装；Linux 的 AppImage 免安装
    let prefs: &[&str] = if cfg!(target_os = "macos") {
        &[".dmg"]
    } else if cfg!(target_os = "windows") {
        &[".exe", ".msi"]
    } else {
        &[".appimage", ".deb", ".rpm"]
    };
    prefs.iter().find_map(|p| {
        assets
            .iter()
            .find(|a| a.name.to_ascii_lowercase().ends_with(p))
    })
}

/// 检查是否有新版本（GET /releases/latest）
#[tauri::command(rename = "app_update_check")]
pub async fn app_update_check(app: AppHandle) -> Result<UpdateInfo, String> {
    let client = build_client(&app)?;
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("检查更新失败: {e}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err("仓库还没有发布任何 Release".into());
    }
    if !status.is_success() {
        return Err(format!("检查更新失败: HTTP {}", status.as_u16()));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;

    let tag = v
        .get("tag_name")
        .and_then(|s| s.as_str())
        .unwrap_or_default();
    let latest = tag
        .trim_start_matches(|c| c == 'v' || c == 'V')
        .to_string();
    let current = current_version(&app);
    let assets: Vec<UpdateAsset> = v
        .get("assets")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    Some(UpdateAsset {
                        name: a.get("name")?.as_str()?.to_string(),
                        url: a.get("browser_download_url")?.as_str()?.to_string(),
                        size: a.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(UpdateInfo {
        has_update: !latest.is_empty() && is_newer(&latest, &current),
        current,
        latest,
        name: v
            .get("name")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string(),
        notes: v
            .get("body")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string(),
        published_at: v
            .get("published_at")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string(),
        html_url: v
            .get("html_url")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string(),
        asset: pick_asset(&assets).cloned(),
    })
}

/// 只保留文件名部分，避免 URL/用户输入里的路径穿越
fn safe_file_name(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim()
        .to_string();
    let cleaned: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+' | ' '))
        .collect();
    if cleaned.is_empty() {
        "wallpaperem-update".into()
    } else {
        cleaned
    }
}

/// 下载安装包到缓存目录 `updates/`，边下边发 `update:progress` 事件，返回落地路径
#[tauri::command(rename = "app_update_download")]
pub async fn app_update_download(
    app: AppHandle,
    url: String,
    name: String,
) -> Result<String, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("updates");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("创建下载目录失败: {e}"))?;
    let dest: PathBuf = dir.join(safe_file_name(&name));

    let client = build_client(&app)?;
    let mut resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("下载失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载失败: HTTP {}", resp.status().as_u16()));
    }
    let total = resp.content_length().unwrap_or(0);

    // 覆盖旧文件：重新下载应拿到完整的新包，而不是续写
    let _ = tokio::fs::remove_file(&dest).await;
    let mut file = tokio::fs::File::create(&dest)
        .await
        .map_err(|e| format!("写入失败: {e}"))?;

    let mut received: u64 = 0;
    let mut last_emit: u64 = 0;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("下载中断: {e}"))? {
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("写入失败: {e}"))?;
        received += chunk.len() as u64;
        // 每 ~256KB 或最后一包推一次，避免小水管下事件风暴
        if received - last_emit >= 256 * 1024 || (total > 0 && received >= total) {
            last_emit = received;
            let _ = app.emit(
                "update:progress",
                json!({ "received": received, "total": total }),
            );
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);

    if total > 0 && received != total {
        return Err(format!("下载不完整：{received}/{total} 字节"));
    }
    let _ = app.emit(
        "update:progress",
        json!({ "received": received, "total": total.max(received) }),
    );
    Ok(dest.to_string_lossy().to_string())
}

/// 打开已下载的安装包，返回一句给用户看的操作提示
#[tauri::command(rename = "app_update_open")]
pub fn app_update_open(path: String) -> Result<String, String> {
    let p = PathBuf::from(&path);
    if !p.is_file() {
        return Err("安装包不存在（可能已被清理），请重新下载".into());
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&p)
            .spawn()
            .map_err(|e| format!("打开安装镜像失败: {e}"))?;
        return Ok("已打开安装镜像：把 WallpaperEM 拖入「应用程序」覆盖旧版即可".into());
    }

    #[cfg(target_os = "windows")]
    {
        // cmd /C start "" <path>：第一个空引号是窗口标题占位，否则带空格路径会被当成标题
        std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(&p)
            .spawn()
            .map_err(|e| format!("启动安装程序失败: {e}"))?;
        return Ok("已启动安装程序，按提示完成更新".into());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // AppImage 免安装：补可执行位后打开所在目录，让用户替换旧文件
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&p) {
                let mut perm = meta.permissions();
                perm.set_mode(perm.mode() | 0o111);
                let _ = std::fs::set_permissions(&p, perm);
            }
        }
        let dir = p.parent().unwrap_or(std::path::Path::new("."));
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
        // 必须显式 return：本块是语句块，尾表达式的值会被丢弃（macOS 上该块被
        // cfg 掉所以不报错，Linux 编译才会撞 E0308）
        if p.to_string_lossy().to_ascii_lowercase().ends_with(".appimage") {
            return Ok("AppImage 已下载并设为可执行，请用它替换旧文件".into());
        }
        return Ok("已打开安装包所在目录，请按发行版方式安装".into());
    }

    #[allow(unreachable_code)]
    Err("当前平台不支持自动打开安装包，请手动安装".into())
}
