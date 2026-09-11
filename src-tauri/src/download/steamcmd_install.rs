//! steamcmd 运行时安装器：下载 → 解压 → 去隔离 → 预热自更新。
//!
//! 安装位置 `<app_data>/steamcmd/`，登录态隔离目录 `<app_data>/steamcmd-home/`。
//!
//! 关于架构：Valve CDN 上的 `steamcmd_osx.tar.gz` 里的引导二进制至今仍是 2020 年的
//! 纯 x86_64，首次运行需要 Rosetta 2；它会立刻自更新，把自己替换成含原生 arm64 slice
//! 的 universal 二进制。所以只有「首次预热」这一步依赖 Rosetta，之后都是原生执行。
//! 因此安装前必须先做 Rosetta 检测，否则子进程会无声死掉。
//!
//! Linux 用 `steamcmd_linux.tar.gz`（x86_64 原生，无 Rosetta 概念），
//! 预热产物是 linux32/linux64 下的 steamclient.so。

use std::path::{Path, PathBuf};

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

/// 官方源与镜像（内容一致，主源失败时回退）
#[cfg(target_os = "macos")]
const TARBALL_URLS: [&str; 2] = [
    "https://media.steampowered.com/client/installer/steamcmd_osx.tar.gz",
    "https://steamcdn-a.akamaihd.net/client/installer/steamcmd_osx.tar.gz",
];
#[cfg(target_os = "linux")]
const TARBALL_URLS: [&str; 2] = [
    "https://media.steampowered.com/client/installer/steamcmd_linux.tar.gz",
    "https://steamcdn-a.akamaihd.net/client/installer/steamcmd_linux.tar.gz",
];
#[cfg(target_os = "windows")]
const TARBALL_URLS: [&str; 2] = [
    "https://media.steampowered.com/client/installer/steamcmd.zip",
    "https://steamcdn-a.akamaihd.net/client/installer/steamcmd.zip",
];

/// 下载包落盘文件名（仅日志/排障可读性用）
#[cfg(target_os = "macos")]
const TARBALL_NAME: &str = "steamcmd_osx.tar.gz";
#[cfg(target_os = "linux")]
const TARBALL_NAME: &str = "steamcmd_linux.tar.gz";
#[cfg(target_os = "windows")]
const TARBALL_NAME: &str = "steamcmd.zip";

/// 引导可执行文件名（*nix 是 steamcmd.sh 包装脚本，Windows 是 steamcmd.exe）
#[cfg(target_os = "windows")]
fn script_file_name() -> &'static str {
    "steamcmd.exe"
}
#[cfg(not(target_os = "windows"))]
fn script_file_name() -> &'static str {
    "steamcmd.sh"
}

/// 自更新写下的版本 manifest 名（平台各一份）
#[cfg(target_os = "macos")]
fn manifest_file_name() -> &'static str {
    "steam_cmd_osx.manifest"
}
#[cfg(target_os = "linux")]
fn manifest_file_name() -> &'static str {
    "steam_cmd_linux.manifest"
}
#[cfg(target_os = "windows")]
fn manifest_file_name() -> &'static str {
    "steam_cmd_win.manifest"
}
/// 预热（首次自更新）超时：需要下载约 85MB 的运行时组件
const WARMUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);
/// 已知的引导包大小量级，用于粗略校验下载完整性（实际约 2.5MB）
const MIN_TARBALL_BYTES: usize = 512 * 1024;

pub const SETTING_VERSION: &str = "steamcmd_version";
pub const SETTING_LOGGED_IN: &str = "steamcmd_logged_in";

pub fn install_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("steamcmd"))
}

/// 子进程的 HOME，隔离 steamcmd 的登录态与配置，避免污染用户真实 Steam 配置
pub fn home_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("steamcmd-home"))
}

pub fn script_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(install_dir(app)?.join(script_file_name()))
}

/// 子进程「家目录」环境变量名。
///
/// steamcmd 没有 configdir 参数，登录态固定写在用户主目录下
/// （macOS/Linux：`$HOME/.steam` 等；Windows：`%USERPROFILE%\AppData\Local\Steam`），
/// 覆盖对应的家目录变量即可把它关进应用自己的目录。
pub fn home_env_key() -> &'static str {
    if cfg!(windows) {
        "USERPROFILE"
    } else {
        "HOME"
    }
}

pub fn is_installed(app: &AppHandle) -> bool {
    script_path(app).map(|p| p.is_file()).unwrap_or(false)
}

/// 是否已完成首次自更新（预热后会出现平台运行时库）
pub fn is_warmed(app: &AppHandle) -> bool {
    install_dir(app).map(|d| is_warmed_dir(&d)).unwrap_or(false)
}

/// Apple Silicon 上是否缺少 Rosetta 2。
///
/// steamcmd 的引导二进制是 x86_64，没有 Rosetta 时 exec 会直接失败且没有有用的报错。
#[cfg(target_os = "macos")]
pub fn rosetta_missing() -> bool {
    if std::env::consts::ARCH != "aarch64" {
        return false;
    }
    // Rosetta 2 安装后会存在该运行时目录；oahd 是其守护进程
    !Path::new("/Library/Apple/usr/libexec/oah").exists()
        && !Path::new("/Library/Apple/usr/share/rosetta").exists()
}

/// Rosetta 是 macOS 概念，其余平台恒 false
#[cfg(not(target_os = "macos"))]
pub fn rosetta_missing() -> bool {
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Download,
    Extract,
    Warmup,
}

impl Phase {
    fn as_str(&self) -> &'static str {
        match self {
            Phase::Download => "download",
            Phase::Extract => "extract",
            Phase::Warmup => "warmup",
        }
    }
}

fn emit(app: &AppHandle, phase: Phase, progress: f64, message: &str) {
    let _ = app.emit(
        "steamcmd:install-progress",
        json!({ "phase": phase.as_str(), "progress": progress, "message": message }),
    );
}

/// 完整安装流程。已安装且已预热时直接返回，实现幂等。
pub async fn install(app: AppHandle, force: bool) -> Result<serde_json::Value, String> {
    if !force && is_installed(&app) && is_warmed(&app) {
        return Ok(json!({ "installed": true, "skipped": true }));
    }
    if rosetta_missing() {
        return Err(
            "ROSETTA_REQUIRED|steamcmd 的官方引导程序是 x86_64 版本，首次启动需要 Rosetta 2。\
             请在终端执行：softwareupdate --install-rosetta --agree-to-license，完成后重试。\
             （首次自更新后 steamcmd 会切换为原生 arm64 运行）"
                .into(),
        );
    }

    let dir = install_dir(&app)?;
    let home = home_dir(&app)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建安装目录失败: {e}"))?;
    std::fs::create_dir_all(&home).map_err(|e| format!("创建配置目录失败: {e}"))?;

    // 1. 下载引导包
    emit(&app, Phase::Download, 0.0, "正在下载 steamcmd…");
    let bytes = download_tarball(&app).await?;
    let tarball = dir.join(TARBALL_NAME);
    std::fs::write(&tarball, &bytes).map_err(|e| format!("写入安装包失败: {e}"))?;
    emit(&app, Phase::Download, 100.0, "下载完成");

    // 2. 解压：用系统 bsdtar，省掉 flate2 + tar 两个依赖
    emit(&app, Phase::Extract, 0.0, "正在解压…");
    extract(&tarball, &dir).await?;
    let _ = std::fs::remove_file(&tarball);

    let script = dir.join(script_file_name());
    if !script.is_file() {
        return Err(format!(
            "解压后未找到 {}，安装包可能损坏",
            script_file_name()
        ));
    }
    #[cfg(target_os = "macos")]
    {
        make_executable(&script)?;
        make_executable(&dir.join("steamcmd"))?;
        // 去掉下载隔离属性，否则 Gatekeeper 会拦截执行
        let _ = tokio::process::Command::new("/usr/bin/xattr")
            .arg("-dr")
            .arg("com.apple.quarantine")
            .arg(&dir)
            .output()
            .await;
    }
    #[cfg(target_os = "linux")]
    {
        make_executable(&script)?;
        // Linux 引导包的真实二进制在 linux32/ 下
        make_executable(&dir.join("linux32").join("steamcmd"))?;
        // 官方 Linux 引导程序仍是 32 位 x86：纯 64 位系统需要 multilib 运行时，
        // 缺了的话 exec 直接报 "No such file or directory"（解释器缺失），提前给出可操作的提示
        if let Some(hint) = multilib_missing_hint(&dir) {
            return Err(hint);
        }
    }
    emit(&app, Phase::Extract, 100.0, "解压完成");

    // 3. 预热：跑一次 +quit 触发自更新（x86_64 引导 → universal，之后原生 arm64）
    emit(
        &app,
        Phase::Warmup,
        0.0,
        "正在初始化（首次运行会自动更新，约需数分钟）…",
    );
    warmup(&app, &script, &home).await?;
    emit(&app, Phase::Warmup, 100.0, "初始化完成");

    let version = read_version(&dir).unwrap_or_else(|| "unknown".into());
    if let Some(db) = app.try_state::<std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>>() {
        if let Ok(conn) = db.lock() {
            let _ = crate::db::set_setting(&conn, SETTING_VERSION, &version);
        }
    }
    tracing::info!(
        "steamcmd installed at {} (version {version})",
        dir.display()
    );
    Ok(json!({ "installed": true, "path": dir.display().to_string(), "version": version }))
}

async fn download_tarball(app: &AppHandle) -> Result<Vec<u8>, String> {
    let proxy = super::read_proxy(app);
    let mut last_err = String::new();
    for (i, url) in TARBALL_URLS.iter().enumerate() {
        if i > 0 {
            emit(app, Phase::Download, 0.0, "主源失败，正在尝试备用镜像…");
        }
        match fetch(url, proxy.as_deref()).await {
            Ok(b) if b.len() >= MIN_TARBALL_BYTES => return Ok(b),
            Ok(b) => {
                last_err = format!("下载内容异常（仅 {} 字节）", b.len());
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!("下载 steamcmd 失败：{last_err}"))
}

async fn fetch(url: &str, proxy: Option<&str>) -> Result<Vec<u8>, String> {
    let mut builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(120));
    if let Some(p) = proxy {
        let px = reqwest::Proxy::all(p).map_err(|e| format!("代理配置无效: {e}"))?;
        builder = builder.proxy(px);
    }
    let client = builder.build().map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    Ok(resp.bytes().await.map_err(|e| e.to_string())?.to_vec())
}

/// *nix：用系统 bsdtar，省掉 flate2 + tar 两个依赖
#[cfg(not(target_os = "windows"))]
async fn extract(tarball: &Path, dest: &Path) -> Result<(), String> {
    let out = tokio::process::Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg(tarball)
        .arg("-C")
        .arg(dest)
        .output()
        .await
        .map_err(|e| format!("调用 tar 失败: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "解压失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Windows：官方包是 zip，用已在依赖里的 zip crate 解压（不再引外部工具）
#[cfg(target_os = "windows")]
async fn extract(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let zip_path = zip_path.to_path_buf();
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let file = std::fs::File::open(&zip_path).map_err(|e| format!("打开安装包失败: {e}"))?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|e| format!("解析安装包失败: {e}"))?;
        // extract() 内部用 enclosed_name 做路径穿越防护
        archive
            .extract(&dest)
            .map_err(|e| format!("解压失败: {e}"))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("解压任务失败: {e}"))?
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if !path.exists() {
        return Ok(());
    }
    let mut perm = std::fs::metadata(path)
        .map_err(|e| e.to_string())?
        .permissions();
    perm.set_mode(perm.mode() | 0o755);
    std::fs::set_permissions(path, perm).map_err(|e| format!("设置可执行权限失败: {e}"))
}

/// 检测 32 位 multilib 缺失（Linux x86_64，引导包只有 linux32 二进制时）。
/// 返回 Some(用户可操作的错误文案) 或 None（可以运行）。
#[cfg(target_os = "linux")]
fn multilib_missing_hint(dir: &Path) -> Option<String> {
    if std::env::consts::ARCH != "x86_64" {
        // ARM 等架构连 x86 模拟层都没有，官方 Linux steamcmd 直接不可用
        return Some(format!(
            "steamcmd 官方 Linux 版本仅支持 x86（32 位），当前架构（{}）无法运行。",
            std::env::consts::ARCH
        ));
    }
    // 已有 64 位运行时（新版 steamcmd 自带 linux64）则不依赖 multilib
    if dir.join("linux64").join("steamcmd").exists() {
        return None;
    }
    // 32 位 ELF 解释器：glibc 系统在 /lib/ld-linux.so.2
    let loader_missing =
        !Path::new("/lib/ld-linux.so.2").exists() && !Path::new("/lib32/ld-linux.so.2").exists();
    if loader_missing {
        return Some(
            "MULTILIB_REQUIRED|steamcmd 官方 Linux 引导程序是 32 位 x86 版本，需要 32 位运行时库。             Debian/Ubuntu：sudo dpkg --add-architecture i386 && sudo apt update &&              sudo apt install libc6:i386 libstdc++6:i386；             Fedora：sudo dnf install glibc.i686 libstdc++.i686；             Arch：启用 multilib 仓库后 sudo pacman -S lib32-glibc lib32-gcc-libs。             安装完成后重试。"
                .into(),
        );
    }
    None
}

/// 首次运行 `steamcmd.sh +quit`，触发自更新下载运行时组件。
///
/// steamcmd 会在自更新后以退出码 42（MAGIC_RESTART_EXITCODE）重启自己，
/// steamcmd.sh 内部处理了重启，所以这里只需等脚本整体结束。
async fn warmup(app: &AppHandle, script: &Path, home: &Path) -> Result<(), String> {
    // 预热也需要交互通道：*nix 上无 TTY 时 steamcmd 可能提前判定为非交互模式而异常退出
    let mut pty =
        crate::download::pty::Pty::open().map_err(|e| format!("分配交互通道失败: {e}"))?;
    let mut cmd = tokio::process::Command::new(script);
    cmd.arg("+quit").env(home_env_key(), home);
    if let Some(p) = super::read_proxy(app) {
        cmd.env("http_proxy", &p)
            .env("https_proxy", &p)
            .env("HTTP_PROXY", &p)
            .env("HTTPS_PROXY", &p);
    }
    pty.attach(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| {
        let base = format!("启动 steamcmd 失败: {e}");
        // 32 位解释器缺失时 exec 报 ENOENT，文案对普通用户没有指向性，补一句
        #[cfg(target_os = "linux")]
        if e.kind() == std::io::ErrorKind::NotFound {
            return format!(
                "{base}。可能是缺少 32 位运行时：Debian/Ubuntu 执行                  sudo dpkg --add-architecture i386 && sudo apt install libc6:i386 libstdc++6:i386"
            );
        }
        base
    })?;
    // 预热不需要写入（+quit 已在命令行上），但写出端必须活到子进程结束：
    // *nix 上它就是 PTY master，提前 drop 等于关掉终端，会把 steamcmd 挂断
    let (_writer, mut out_rx) = pty
        .channel(&mut child, crate::download::backend::is_prompt)
        .await
        .map_err(|e| format!("建立交互通道失败: {e}"))?;

    let pump = async {
        let mut tail = String::new();
        while let Some(ev) = out_rx.recv().await {
            let line = match ev {
                crate::download::pty::OutEvent::Line(l) => l,
                crate::download::pty::OutEvent::Prompt(p) => p,
            };
            tail.push_str(&line);
            tail.push('\n');
            if tail.len() > 4096 {
                let cut = tail.len() - 2048;
                tail = tail[cut..].to_string();
            }
            // 自更新阶段 steamcmd 会打印下载百分比，转发给前端做进度提示
            let l = line.trim();
            if !l.is_empty() {
                emit(app, Phase::Warmup, 50.0, l);
            }
        }
        tail
    };

    let tail = match tokio::time::timeout(WARMUP_TIMEOUT, pump).await {
        Ok(t) => t,
        Err(_) => {
            let _ = child.start_kill();
            return Err("steamcmd 初始化超时（10 分钟），请检查网络或代理后重试".into());
        }
    };
    // 预热完成后进程可能因已知的退出 hang 而不结束，给一个短等待再收尾
    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), child.wait()).await;
    let _ = child.start_kill();

    if !is_warmed_dir(script.parent().unwrap_or(Path::new("."))) {
        return Err(format!(
            "steamcmd 初始化未完成（未生成运行时组件）。最近输出：{}",
            tail.lines()
                .rev()
                .filter(|l| !l.trim().is_empty())
                .take(5)
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn is_warmed_dir(dir: &Path) -> bool {
    dir.join("steamclient.dylib").exists()
}

/// Linux 预热产物：自更新解出的 steamclient.so（32/64 位两套，任一出现即算完成）
#[cfg(target_os = "linux")]
fn is_warmed_dir(dir: &Path) -> bool {
    dir.join("linux32").join("steamclient.so").exists()
        || dir.join("linux64").join("steamclient.so").exists()
        || dir.join("steamclient.so").exists()
}

/// Windows 预热产物：自更新解出的 steamclient(dll)（64 位包为主，两套任一出现即算完成）
#[cfg(target_os = "windows")]
fn is_warmed_dir(dir: &Path) -> bool {
    dir.join("steamclient.dll").exists() || dir.join("steamclient64.dll").exists()
}

/// 从自更新写下的 manifest 里读版本号
fn read_version(dir: &Path) -> Option<String> {
    let manifest = dir.join("package").join(manifest_file_name());
    let text = std::fs::read_to_string(manifest).ok()?;
    let re = regex::Regex::new(r#""version"\s+"(\d+)""#).ok()?;
    re.captures(&text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// 卸载：删除安装目录（登录态目录另行保留，由「登出」清理）
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
    Ok(())
}

/// 安装状态摘要，供设置页展示
pub fn status(app: &AppHandle) -> serde_json::Value {
    let installed = is_installed(app);
    let warmed = is_warmed(app);
    let version = app
        .try_state::<std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>>()
        .and_then(|db| {
            let conn = db.lock().ok()?;
            crate::db::get_setting(&conn, SETTING_VERSION)
        });
    json!({
        "installed": installed && warmed,
        "downloaded": installed,
        "warmed": warmed,
        "path": install_dir(app).map(|p| p.display().to_string()).unwrap_or_default(),
        "version": version,
        "rosettaMissing": rosetta_missing(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rosetta_check_only_applies_to_apple_silicon() {
        // 非 arm64 平台永远不应报缺失
        if std::env::consts::ARCH != "aarch64" {
            assert!(!rosetta_missing());
        }
    }

    #[test]
    fn version_parsed_from_manifest() {
        let dir = std::env::temp_dir().join("wpem-steamcmd-manifest-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("package")).unwrap();
        std::fs::write(
            dir.join("package").join(manifest_file_name()),
            "\"steam_cmd\"\n{\n\t\"version\"\t\"1788292693\"\n}\n",
        )
        .unwrap();
        assert_eq!(read_version(&dir).as_deref(), Some("1788292693"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_absent_when_manifest_missing() {
        let dir = std::env::temp_dir().join("wpem-steamcmd-nomanifest-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_version(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
