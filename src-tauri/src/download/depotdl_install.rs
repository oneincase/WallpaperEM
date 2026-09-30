//! DepotDownloader 运行时安装器：下载 → 解压 → 自检。
//!
//! 安装位置 `<app_data>/depotdownloader/`，登录态隔离目录 `<app_data>/depotdl-home/`。
//!
//! 为什么不打包 sidecar：上游 Release 每个平台 30 MB 左右（.NET 9 自带运行时的
//! 单文件），六个平台全打包会让安装包膨胀上百 MB，而它只是「可选下载工具」。
//! 与 steamcmd / ffmpeg 托管副本同一策略：要用时从官方源拉一次。
//!
//! 许可：DepotDownloader 是第三方开源项目（GPL-2.0）。我们只在自己的运行时里
//! 从上游官方 Release 下载并调用它，不随本应用分发其二进制；解压后保留上游
//! LICENSE 文件，设置页也标注许可与来源。
//!
//! macOS 注意：上游二进制是 ad-hoc 签名的 arm64/x64 单文件（实测可直接执行，
//! 不需要 Rosetta，也不需要随宿主签名），但下载后仍要清 quarantine 属性 ——
//! 有该属性的第三方二进制会被 Gatekeeper 拦下且没有可用报错。

use std::path::{Path, PathBuf};

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

/// 上游 Release 版本（tag 形如 `DepotDownloader_3.4.0`）
pub const VERSION: &str = "3.4.0";
/// 下载体积量级（设置页文案用；各平台 30–33 MB）
pub const EXPECTED_DOWNLOAD_BYTES: u64 = 33 * 1024 * 1024;
/// 包完整性下限（真实包 ~30 MB，明显偏小说明是错误页/半截包）
const MIN_ZIP_BYTES: u64 = 20 * 1024 * 1024;
/// 自检超时：单文件 .NET 首次启动要解压/布局运行时，给足秒数
const SELFCHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

pub const SETTING_VERSION: &str = "depotdl_version";
pub const SETTING_LOGGED_IN: &str = "depotdl_logged_in";
pub const EVENT_PROGRESS: &str = "depotdl:install-progress";

/// 平台 → 上游 asset 名。不支持的架构返回 None（设置页据此隐藏安装入口）。
pub fn asset_name() -> Option<&'static str> {
    let arch = std::env::consts::ARCH;
    if cfg!(target_os = "macos") {
        match arch {
            "aarch64" => Some("macos-arm64"),
            "x86_64" => Some("macos-x64"),
            _ => None,
        }
    } else if cfg!(target_os = "linux") {
        match arch {
            "aarch64" => Some("linux-arm64"),
            "x86_64" => Some("linux-x64"),
            _ => None,
        }
    } else if cfg!(target_os = "windows") {
        match arch {
            "aarch64" => Some("windows-arm64"),
            "x86_64" => Some("windows-x64"),
            _ => None,
        }
    } else {
        None
    }
}

fn download_url(asset: &str) -> String {
    format!(
        "https://github.com/SteamRE/DepotDownloader/releases/download/DepotDownloader_{VERSION}/DepotDownloader-{asset}.zip"
    )
}

/// 可执行文件名（上游包内文件名，解压后按名查找而不是写死层级）
pub fn bin_file_name() -> &'static str {
    if cfg!(windows) {
        "DepotDownloader.exe"
    } else {
        "DepotDownloader"
    }
}

pub fn install_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("depotdownloader"))
}

/// 登录态隔离目录（HOME/XDG 重定向目标；登出时整目录删除）
pub fn home_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("depotdl-home"))
}

pub fn bin_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(install_dir(app)?.join(bin_file_name()))
}

pub fn is_installed(app: &AppHandle) -> bool {
    bin_path(app).map(|p| p.is_file()).unwrap_or(false)
}

fn emit(app: &AppHandle, phase: &str, progress: f64, message: &str) {
    let _ = app.emit(
        EVENT_PROGRESS,
        json!({ "phase": phase, "progress": progress, "message": message }),
    );
}

/// 完整安装流程（幂等：已安装且能跑起来时直接返回）
pub async fn install(app: AppHandle, force: bool) -> Result<serde_json::Value, String> {
    let Some(asset) = asset_name() else {
        return Err(format!(
            "当前平台（{}-{}）没有 DepotDownloader 官方构建，请改用 steamcmd",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
    };
    let dir = install_dir(&app)?;
    let bin = bin_path(&app)?;
    if !force {
        if let Some(v) = probe(&bin).await {
            return Ok(json!({ "installed": true, "skipped": true, "version": v }));
        }
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建安装目录失败: {e}"))?;
    let tmp = dir.join("tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("创建临时目录失败: {e}"))?;

    // 1. 下载（流式写盘 + 百分比进度）
    emit(&app, "download", 0.0, "正在下载 DepotDownloader…");
    let zip_path = tmp.join("DepotDownloader.zip");
    if let Err(e) = download_to(&app, &download_url(asset), &zip_path).await {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!(
            "下载 DepotDownloader 失败：{e}。若网络受限，可在「设置 → 网络与服务」配置代理后重试"
        ));
    }
    emit(&app, "download", 100.0, "下载完成");

    // 2. 解压：包内只有可执行文件 + LICENSE（不假设层级，按文件名找）
    emit(&app, "extract", 0.0, "正在解压…");
    let extract_dir = tmp.join("extract");
    std::fs::create_dir_all(&extract_dir).map_err(|e| format!("创建解压目录失败: {e}"))?;
    if let Err(e) = extract(&zip_path, &extract_dir).await {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let Some(found) = find_file(&extract_dir, bin_file_name()) else {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err("安装包里没找到 DepotDownloader 可执行文件（上游包结构可能变了）".into());
    };
    // Windows 上覆盖正在运行的 exe 会失败，先删旧的
    let _ = std::fs::remove_file(&bin);
    if std::fs::rename(&found, &bin).is_err() {
        if let Err(e) = std::fs::copy(&found, &bin) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(format!("写入 DepotDownloader 失败: {e}"));
        }
    }
    // 上游 LICENSE 一起留在安装目录（GPL-2.0 要求保留许可声明）
    if let Some(lic) = find_file(&extract_dir, "LICENSE") {
        let _ = std::fs::copy(&lic, dir.join("LICENSE"));
    }
    #[cfg(unix)]
    if let Err(e) = make_executable(&bin) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    #[cfg(target_os = "macos")]
    {
        // 去掉下载隔离属性，否则 Gatekeeper 会静默拦下执行
        let _ = tokio::process::Command::new("/usr/bin/xattr")
            .arg("-dr")
            .arg("com.apple.quarantine")
            .arg(&dir)
            .output()
            .await;
    }
    let _ = std::fs::remove_dir_all(&tmp);
    emit(&app, "extract", 100.0, "解压完成");

    // 3. 自检：能跑起来才算装好（顺带把版本写进设置页要用的地方）
    emit(&app, "check", 0.0, "正在校验…");
    let Some(version) = probe(&bin).await else {
        let _ = std::fs::remove_file(&bin);
        return Err(
            "DepotDownloader 已下载但无法执行（安装包不完整，或被安全软件/隔离属性拦截）".into(),
        );
    };
    if let Some(db) = app.try_state::<std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>>() {
        if let Ok(conn) = db.lock() {
            let _ = crate::db::set_setting(&conn, SETTING_VERSION, &version);
        }
    }
    emit(&app, "check", 100.0, "校验通过");
    tracing::info!(
        "DepotDownloader 已安装: {} (version {version})",
        bin.display()
    );
    Ok(json!({
        "installed": true,
        "version": version,
        "path": bin.display().to_string(),
    }))
}

/// 运行 `<bin> --version` 做自检，返回版本号（形如 `3.4.0`）。
/// 输出首行：`DepotDownloader v3.4.0+c553ef4d...`
async fn probe(bin: &Path) -> Option<String> {
    if !bin.is_file() {
        return None;
    }
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("--version");
    crate::util::hide_console_tokio(&mut cmd);
    let fut = cmd.output();
    let out = tokio::time::timeout(SELFCHECK_TIMEOUT, fut)
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_version(&String::from_utf8_lossy(&out.stdout))
}

/// 从 `DepotDownloader v3.4.0+hash` 里取 `3.4.0`
fn parse_version(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.contains("DepotDownloader"))?;
    let rest = line.split(" v").nth(1)?;
    let ver = rest.split('+').next()?.trim();
    (!ver.is_empty()).then(|| ver.to_string())
}

/// 流式下载安装包到 `dest`，进度经 `depotdl:install-progress` 推送。
/// 落盘大小低于 [`MIN_ZIP_BYTES`] 视为错误页/半截包。
async fn download_to(app: &AppHandle, url: &str, dest: &Path) -> Result<u64, String> {
    let proxy = crate::download::read_proxy(app);
    let written = stream_to_file(url, dest, proxy.as_deref(), |pct| {
        emit(app, "download", pct, "正在下载 DepotDownloader…")
    })
    .await?;
    if written < MIN_ZIP_BYTES {
        return Err(format!("下载内容异常（仅 {written} 字节，可能是错误页）"));
    }
    Ok(written)
}

/// 流式下载到 `dest`（百分比回调；总长度未知时不回调），返回写入字节数。
///
/// 与 AppHandle 拆开是为了能在真实网络上单独验证这段流式逻辑
/// （代理与进度上报由调用方提供，其余与安装路径完全一致）。
async fn stream_to_file(
    url: &str,
    dest: &Path,
    proxy: Option<&str>,
    mut on_progress: impl FnMut(f64),
) -> Result<u64, String> {
    use tokio::io::AsyncWriteExt;

    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30 * 60))
        .user_agent("WallpaperEM-DepotDownloader-Installer");
    if let Some(p) = proxy {
        builder = builder.proxy(reqwest::Proxy::all(p).map_err(|e| format!("代理配置无效: {e}"))?);
    }
    let client = builder.build().map_err(|e| e.to_string())?;
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| format!("创建文件失败: {e}"))?;
    let mut written: u64 = 0;
    let mut last_pct = -1.0f64;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("读取失败: {e}"))? {
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("写入失败: {e}"))?;
        written += chunk.len() as u64;
        if total > 0 {
            let pct = written as f64 / total as f64 * 100.0;
            if pct - last_pct >= 1.0 {
                last_pct = pct;
                on_progress(pct);
            }
        }
    }
    file.flush().await.map_err(|e| format!("写入失败: {e}"))?;
    Ok(written)
}

async fn extract(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let zip_path = zip_path.to_path_buf();
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let file = std::fs::File::open(&zip_path).map_err(|e| format!("打开安装包失败: {e}"))?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("解析安装包失败: {e}"))?;
        // extract() 内部用 enclosed_name 做路径穿越防护
        archive
            .extract(&dest)
            .map_err(|e| format!("解压失败: {e}"))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("解压任务失败: {e}"))?
}

/// 在解压目录里按文件名递归找（上游包结构今后变化也能找到）
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_file(&p, name) {
                return Some(found);
            }
        } else if p.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(p);
        }
    }
    None
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path)
        .map_err(|e| e.to_string())?
        .permissions();
    perm.set_mode(perm.mode() | 0o755);
    std::fs::set_permissions(path, perm).map_err(|e| format!("设置可执行权限失败: {e}"))
}

/// 卸载：删除安装目录（登录态目录保留，由「登出」清理）
pub fn uninstall(app: &AppHandle) -> Result<(), String> {
    let dir = install_dir(app)?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("删除失败: {e}"))?;
    }
    if let Some(db) = app.try_state::<std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>>() {
        if let Ok(conn) = db.lock() {
            let _ = conn.execute("DELETE FROM settings WHERE key = ?1", [SETTING_VERSION]);
        }
    }
    tracing::info!("DepotDownloader 已卸载（登录态目录保留）");
    Ok(())
}

/// 安装状态摘要，供设置页展示
pub fn status(app: &AppHandle) -> serde_json::Value {
    let installed = is_installed(app);
    let version = app
        .try_state::<std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>>()
        .and_then(|db| {
            let conn = db.lock().ok()?;
            crate::db::get_setting(&conn, SETTING_VERSION)
        });
    json!({
        "supported": asset_name().is_some(),
        "installed": installed,
        "path": install_dir(app).map(|p| p.display().to_string()).unwrap_or_default(),
        "version": version,
        "license": "GPL-2.0",
        "source": "https://github.com/SteamRE/DepotDownloader",
        "expectedDownloadBytes": EXPECTED_DOWNLOAD_BYTES,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsed_from_probe_output() {
        let out = "DepotDownloader v3.4.0+c553ef4d60c00a4f5fd16c9fe017f569001589ff\n\
                   Runtime: .NET 9.0.4 on Darwin 27.0.0\n";
        assert_eq!(parse_version(out).as_deref(), Some("3.4.0"));
        assert_eq!(parse_version("").as_deref(), None);
        assert_eq!(parse_version("Runtime: .NET 9.0.4").as_deref(), None);
    }

    #[test]
    fn asset_matches_platform() {
        let a = asset_name().expect("主流平台都应有官方构建");
        // 平台段的拼写与 Rust 的 OS 名一致（macos / linux / windows），架构段是 arm64/x64
        assert!(a.starts_with(std::env::consts::OS), "{a}");
        assert!(a.contains("arm64") || a.contains("x64"), "{a}");
    }

    #[test]
    fn download_url_points_at_pinned_release() {
        let u = download_url("macos-arm64");
        assert!(u.starts_with("https://github.com/SteamRE/DepotDownloader/releases/download/"));
        assert!(u.contains(&format!("DepotDownloader_{VERSION}/")));
        assert!(u.ends_with("DepotDownloader-macos-arm64.zip"));
    }

    #[test]
    fn finds_binary_in_nested_layout() {
        let d = std::env::temp_dir().join("wpem-depotdl-find-test");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub").join("DepotDownloader"), "x").unwrap();
        let found = find_file(&d, "DepotDownloader").expect("递归查找应命中");
        assert!(found.ends_with("sub/DepotDownloader"));
        assert!(find_file(&d, "LICENSE").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
