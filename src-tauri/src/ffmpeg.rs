//! 托管 ffmpeg：抽帧（视频 / GIF → 首帧 PNG）的运行时安装器。
//!
//! ## 为什么需要它
//! 有两个功能依赖抽帧，而且都不是「可有可无的装饰」：
//! - **自动设置系统壁纸**：壁纸引擎没运行时，靠这张抽出来的静止帧维持桌面视觉一致；
//! - **本地库卡片封面**：导入的视频壁纸没封面就是一张灰卡（失败只记日志、静默降级）。
//!
//! macOS 走系统 AVFoundation（`AVAssetImageGenerator`），无外部依赖；Linux / Windows
//! 此前要求用户自己装 ffmpeg（`apt install ffmpeg` / `winget install Gyan.FFmpeg`），
//! 而这恰恰是最容易卡住的一步 —— 功能开关打得开、结果什么都不发生。这里把它变成
//! 应用内一键安装。
//!
//! ## 为什么是运行时安装，而不是打进安装包
//! - **体积**：静态构建解压后 80~90MB（压缩包 40~90MB），而它只服务抽帧这一件事；
//! - **许可**：上游静态构建是 GPL，随包分发就要一并履行源码 / 许可义务；让用户从
//!   上游官方地址直接下载则不涉及再分发；
//! - **一致性**：与 steamcmd 完全同一套范式（下载 → 校验大小 → 解压 → 落进应用数据
//!   目录 → 设置页可见状态与进度、可卸载），用户心智与代码路径都只需要理解一次。
//!
//! ## 查找顺序
//! 托管副本（`<app_data>/ffmpeg/ffmpeg[.exe]`）优先，其次才是 PATH 上的系统 ffmpeg。
//! 理由：应用内装过就以它为准（静态构建、行为确定、不受用户 PATH 里同名程序影响）；
//! 没装过则直接用系统已有的（发行版包更贴合本机，也省一次下载）。

// macOS 上只有桩实现（抽帧走 AVFoundation），下面的常量与缓存没有使用点
#![cfg_attr(not(any(target_os = "windows", target_os = "linux")), allow(dead_code))]

use std::path::PathBuf;
use std::sync::OnceLock;

use tauri::AppHandle;

/// 安装进度事件（设置页监听；phase 为 `download` / `extract`）
const EVENT_PROGRESS: &str = "ffmpeg:install-progress";
/// 已安装版本（写进设置表，供排障与「当前用的是哪一份」）
pub const SETTING_VERSION: &str = "ffmpeg_version";
/// 托管目录名（位于应用数据目录下）
const DIR_NAME: &str = "ffmpeg";
/// 下载 + 解压的临时目录名（与托管二进制同目录，装完即删；同盘改名不做跨盘拷贝）
const TMP_NAME: &str = "tmp";
/// 下载体积下限：上游包 40~90MB，这里只用来挡住「拿到一个 HTML 错误页」这类假成功
const MIN_ARCHIVE_BYTES: u64 = 5 * 1024 * 1024;
/// HTTP 客户端标识（部分上游对无 UA 的请求会 403）
const USER_AGENT: &str = concat!("WallpaperEM/", env!("CARGO_PKG_VERSION"));

/// 抽帧时找不到 ffmpeg 的收尾提示（Linux / Windows 共用；macOS 不走 ffmpeg 故不需要）
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub const MISSING_HINT: &str =
    "请在 设置 → 通用 →「抽帧组件（ffmpeg）」点「安装」（或自行安装 ffmpeg 后重试）";

/// 托管副本的绝对路径。
///
/// 抽帧入口（`system_wallpaper` 的平台实现、`library` 导入）拿不到 AppHandle，
/// 所以路径在启动时由 [`init`] 缓存下来；安装成功时也会补写，无需重启即生效。
static MANAGED_BIN: OnceLock<PathBuf> = OnceLock::new();

/// 抽帧要用的命令：优先托管副本，其次 PATH 上的系统 ffmpeg。
///
/// 每次调用都判一次 `is_file()`：用户手动删掉托管副本后自动回落系统 ffmpeg，
/// 不会拿着一个不存在的路径反复报「启动失败」。
pub fn command() -> std::process::Command {
    let mut cmd = match MANAGED_BIN.get().filter(|p| p.is_file()) {
        Some(p) => std::process::Command::new(p),
        None => std::process::Command::new("ffmpeg"),
    };
    // ffmpeg 是控制台程序：不加这个，Windows 上每次抽帧都会闪一个黑窗
    crate::util::hide_console(&mut cmd);
    cmd
}

/// 从 `ffmpeg -version` 首行取版本号：
/// `ffmpeg version 7.1.1-essentials_build-www.gyan.dev Copyright (c) 2000-2024` → `7.1.1-...`
fn parse_version(stdout: &str) -> Option<String> {
    let first = stdout.lines().next()?;
    first
        .strip_prefix("ffmpeg version ")?
        .split_whitespace()
        .next()
        .map(|s| s.to_string())
}

/// 解压上游包。
///
/// 按**扩展名**而不是按平台分派：Windows 上游给 zip（用依赖里已有的 zip crate），
/// Linux 上游给 tar.xz（用系统 tar）。这样两条路径在任何平台都能编译、也就能被单测覆盖
/// —— 解压 + 定位二进制是整个安装流程里最容易出错的一段（各上游包的目录结构都不一样）。
async fn extract(archive: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    let is_zip = archive
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("zip"))
        .unwrap_or(false);
    if is_zip {
        let (archive, dest) = (archive.to_path_buf(), dest.to_path_buf());
        return tokio::task::spawn_blocking(move || -> Result<(), String> {
            let file =
                std::fs::File::open(&archive).map_err(|e| format!("打开安装包失败: {e}"))?;
            let mut zip =
                zip::ZipArchive::new(file).map_err(|e| format!("解析安装包失败: {e}"))?;
            // extract() 内部用 enclosed_name 挡住 ../ 路径穿越
            zip.extract(&dest).map_err(|e| format!("解压失败: {e}"))?;
            Ok(())
        })
        .await
        .map_err(|e| format!("解压任务失败: {e}"))?;
    }
    let mut tar = tokio::process::Command::new("tar");
    tar.arg("-xJf").arg(archive).arg("-C").arg(dest);
    crate::util::hide_console_tokio(&mut tar);
    let out = tar
        .output()
        .await
        .map_err(|e| format!("调用 tar 失败: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        // xz 缺失是最常见的失败原因（最小化安装的发行版），给一句能照做的提示
        if err.contains("xz") || err.contains("XZ") {
            return Err(
                "解压失败：缺少 xz（请先 sudo apt install xz-utils / sudo dnf install xz）".into(),
            );
        }
        return Err(format!("解压失败: {}", err.trim()));
    }
    Ok(())
}

/// 在上游包里找 ffmpeg 本体（平台无关：只按文件名找，调用方给 `ffmpeg` / `ffmpeg.exe`）。
///
/// 优先 `bin/ffmpeg[.exe]`（GitHub 构建的布局），否则取任意同名的普通文件
/// （johnvansickle 静态包把 ffmpeg 直接放在解压根目录）。深度限制 3 层：
/// 两个上游包的实际层级都是 1~2 层，再深只可能是我们理解错了包结构。
fn find_binary(root: &std::path::Path, bin_name: &str) -> Option<PathBuf> {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut fallback = None;
    while let Some((dir, depth)) = stack.pop() {
        if depth > 3 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push((path, depth + 1));
                continue;
            }
            if !ft.is_file() || path.file_name().and_then(|n| n.to_str()) != Some(bin_name) {
                continue;
            }
            let in_bin = path
                .parent()
                .and_then(|d| d.file_name())
                .and_then(|n| n.to_str())
                == Some("bin");
            if in_bin {
                return Some(path);
            }
            if fallback.is_none() {
                fallback = Some(path);
            }
        }
    }
    fallback
}

pub use imp::{init, install, status, uninstall};

/// 抽帧组件状态（设置页据此渲染「未安装 / 已安装 / 系统已装」与体积、路径）
#[tauri::command]
pub fn ffmpeg_status(app: AppHandle) -> serde_json::Value {
    status(&app)
}

/// 安装 / 修复托管副本。`force` = true 时即使已装好也重新下载（修复用）。
#[tauri::command]
pub async fn ffmpeg_install(app: AppHandle, force: Option<bool>) -> Result<serde_json::Value, String> {
    install(app, force.unwrap_or(false)).await
}

/// 卸载托管副本（只删应用自己下的那一份，绝不碰系统 ffmpeg）
#[tauri::command]
pub fn ffmpeg_uninstall(app: AppHandle) -> Result<(), String> {
    uninstall(&app)
}

// ---------- Windows / Linux：真正的安装实现 ----------

#[cfg(any(target_os = "windows", target_os = "linux"))]
mod imp {
    use super::*;
    use std::path::Path;

    use serde_json::json;
    use tauri::{Emitter, Manager};

    /// 托管副本的文件名
    #[cfg(target_os = "windows")]
    const BIN_NAME: &str = "ffmpeg.exe";
    #[cfg(target_os = "linux")]
    const BIN_NAME: &str = "ffmpeg";
    /// 下载包落盘名（仅日志 / 排障可读性用）
    #[cfg(target_os = "windows")]
    const ARCHIVE_NAME: &str = "ffmpeg.zip";
    #[cfg(target_os = "linux")]
    const ARCHIVE_NAME: &str = "ffmpeg.tar.xz";

    /// 一个下载源：`label` 用于进度文案，`url` 是上游官方地址
    struct Source {
        label: &'static str,
        url: &'static str,
    }

    /// Windows：上游两个通行的静态构建（winget 的 Gyan.FFmpeg 用的就是前者）。
    /// 都不内置、只下载，所以许可与体积问题留在上游。
    #[cfg(target_os = "windows")]
    const SOURCES: &[Source] = &[
        Source {
            label: "GyanD essentials",
            url: "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip",
        },
        Source {
            label: "BtbN win64-lgpl",
            url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-lgpl.zip",
        },
    ];

    /// Linux x86_64：johnvansickle 的静态构建是社区最常用的 Linux 静态包
    /// （单文件、无 glibc 版本要求，解压即得根目录下的 ffmpeg）。
    #[cfg(target_os = "linux")]
    const SOURCES_X86_64: &[Source] = &[
        Source {
            label: "johnvansickle static",
            url: "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz",
        },
        Source {
            label: "BtbN linux64-gpl",
            url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linux64-gpl.tar.xz",
        },
    ];
    /// Linux arm64（如树莓派 / ARM 桌面）
    #[cfg(target_os = "linux")]
    const SOURCES_AARCH64: &[Source] = &[
        Source {
            label: "johnvansickle static",
            url: "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-arm64-static.tar.xz",
        },
        Source {
            label: "BtbN linuxarm64-gpl",
            url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linuxarm64-gpl.tar.xz",
        },
    ];

    #[cfg(target_os = "windows")]
    fn sources() -> &'static [Source] {
        SOURCES
    }
    #[cfg(target_os = "linux")]
    fn sources() -> &'static [Source] {
        if std::env::consts::ARCH == "aarch64" {
            SOURCES_AARCH64
        } else {
            SOURCES_X86_64
        }
    }

    /// 下载体积量级（仅用于设置页文案，避免前端硬编码平台差异）
    #[cfg(target_os = "windows")]
    const EXPECTED_DOWNLOAD_BYTES: u64 = 90 * 1024 * 1024;
    #[cfg(target_os = "linux")]
    const EXPECTED_DOWNLOAD_BYTES: u64 = 40 * 1024 * 1024;

    fn managed_dir(app: &AppHandle) -> Result<PathBuf, String> {
        Ok(app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join(DIR_NAME))
    }

    fn managed_binary(app: &AppHandle) -> Result<PathBuf, String> {
        Ok(managed_dir(app)?.join(BIN_NAME))
    }

    /// 启动时缓存托管副本路径（文件还不存在也先记下：安装成功后无需重启即可生效）
    pub fn init(app: &AppHandle) {
        match managed_binary(app) {
            Ok(p) => {
                if p.is_file() {
                    tracing::info!("ffmpeg: 使用托管副本 {}", p.display());
                    let _ = MANAGED_BIN.set(p);
                }
            }
            Err(e) => tracing::warn!("ffmpeg: 解析托管路径失败: {e}"),
        }
    }

    fn emit(app: &AppHandle, phase: &str, progress: f64, message: &str) {
        let _ = app.emit(
            EVENT_PROGRESS,
            json!({ "phase": phase, "progress": progress, "message": message }),
        );
    }

    pub fn status(app: &AppHandle) -> serde_json::Value {
        let dir = managed_dir(app).ok();
        let bin = dir.as_ref().map(|d| d.join(BIN_NAME));
        let managed = bin.as_ref().map(|p| p.is_file()).unwrap_or(false);
        // 系统 ffmpeg 的探测要走一次进程（没有 ffmpeg 时是一次失败 spawn，代价可忽略）
        let system = system_version();
        let active = if managed {
            Some("managed")
        } else if system.is_some() {
            Some("system")
        } else {
            None
        };
        json!({
            "supported": true,
            "managed": managed,
            "system": system.is_some(),
            "active": active,
            // 托管副本的版本直接跑它自己（不依赖 init 是否已缓存路径）
            "version": match (&bin, managed) {
                (Some(p), true) => probe_sync(p).or_else(|| system.clone()),
                _ => system.clone(),
            },
            "path": bin.as_ref().filter(|p| p.is_file()).map(|p| p.display().to_string()),
            "sizeBytes": bin
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| m.len())
                .unwrap_or(0),
            "dir": dir.as_ref().map(|d| d.display().to_string()),
            "expectedDownloadBytes": EXPECTED_DOWNLOAD_BYTES,
        })
    }

    fn probe_sync(path: &Path) -> Option<String> {
        if !path.is_file() {
            return None;
        }
        let mut cmd = std::process::Command::new(path);
        cmd.arg("-version");
        crate::util::hide_console(&mut cmd);
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        parse_version(&String::from_utf8_lossy(&out.stdout))
    }

    async fn probe(path: &Path) -> Option<String> {
        if !path.is_file() {
            return None;
        }
        let mut cmd = tokio::process::Command::new(path);
        cmd.arg("-version");
        crate::util::hide_console_tokio(&mut cmd);
        let out = cmd.output().await.ok()?;
        if !out.status.success() {
            return None;
        }
        parse_version(&String::from_utf8_lossy(&out.stdout))
    }

    /// 系统 PATH 上的 ffmpeg（用户自己装的）
    fn system_version() -> Option<String> {
        let mut cmd = std::process::Command::new("ffmpeg");
        cmd.arg("-version");
        crate::util::hide_console(&mut cmd);
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        parse_version(&String::from_utf8_lossy(&out.stdout))
    }

    pub async fn install(app: AppHandle, force: bool) -> Result<serde_json::Value, String> {
        let dir = managed_dir(&app)?;
        let bin = dir.join(BIN_NAME);
        if !force {
            if let Some(v) = probe(&bin).await {
                return Ok(json!({ "installed": true, "skipped": true, "version": v }));
            }
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建安装目录失败: {e}"))?;
        let tmp = dir.join(TMP_NAME);
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| format!("创建临时目录失败: {e}"))?;
        let archive = tmp.join(ARCHIVE_NAME);

        // 1. 下载（流式写盘 + 百分比进度；主源失败自动换镜像）
        let mut last_err = String::new();
        let mut ok = false;
        for (i, src) in sources().iter().enumerate() {
            if i > 0 {
                emit(&app, "download", 0.0, "主源失败，正在尝试备用镜像…");
            }
            match download_to(&app, src, &archive).await {
                Ok(n) if n >= MIN_ARCHIVE_BYTES => {
                    ok = true;
                    break;
                }
                Ok(n) => last_err = format!("下载内容异常（仅 {n} 字节，可能是错误页）"),
                Err(e) => last_err = e,
            }
        }
        if !ok {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(format!("下载 ffmpeg 失败：{last_err}"));
        }
        emit(&app, "download", 100.0, "下载完成");

        // 2. 解压（Windows = zip crate；Linux = 系统 tar + xz）
        emit(&app, "extract", 0.0, "正在解压…");
        let extract_dir = tmp.join("extract");
        std::fs::create_dir_all(&extract_dir).map_err(|e| format!("创建解压目录失败: {e}"))?;
        // 失败路径统一清掉临时目录：下载包 90MB，留在应用数据目录里用户既看不懂也删不掉
        if let Err(e) = extract(&archive, &extract_dir).await {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }

        // 3. 取出 ffmpeg 本体落进托管目录。上游包目录层级各版本不一样
        //    （`<ver>/bin/ffmpeg.exe` vs `<ver>-static/ffmpeg`），按文件名找而不是写死路径
        let Some(found) = find_binary(&extract_dir, BIN_NAME) else {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err("安装包里没找到 ffmpeg 可执行文件（上游包结构可能变了）".into());
        };
        // Windows 上覆盖正在运行的 exe 会失败，先删旧的
        let _ = std::fs::remove_file(&bin);
        if std::fs::rename(&found, &bin).is_err() {
            if let Err(e) = std::fs::copy(&found, &bin) {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(format!("写入 ffmpeg 失败: {e}"));
            }
        }
        #[cfg(unix)]
        if let Err(e) = make_executable(&bin) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
        let _ = std::fs::remove_dir_all(&tmp);

        // 4. 自检：能跑起来才算装好（避免「文件在但不可执行」被当成成功）
        let Some(version) = probe(&bin).await else {
            let _ = std::fs::remove_file(&bin);
            return Err("ffmpeg 已下载但无法执行（安装包不完整，或被安全软件拦截）".into());
        };
        // 安装后立即生效：抽帧侧读的是这个缓存路径
        let _ = MANAGED_BIN.set(bin.clone());
        if let Some(db) = app.try_state::<std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>>() {
            if let Ok(conn) = db.lock() {
                let _ = crate::db::set_setting(&conn, SETTING_VERSION, &version);
            }
        }
        tracing::info!("ffmpeg 托管副本已安装: {} (version {version})", bin.display());
        Ok(json!({
            "installed": true,
            "version": version,
            "path": bin.display().to_string(),
        }))
    }

    pub fn uninstall(app: &AppHandle) -> Result<(), String> {
        let dir = managed_dir(app)?;
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("删除 ffmpeg 目录失败: {e}"))?;
        }
        if let Some(db) = app.try_state::<std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>>() {
            if let Ok(conn) = db.lock() {
                let _ = crate::db::set_setting(&conn, SETTING_VERSION, "");
            }
        }
        tracing::info!("ffmpeg 托管副本已卸载（系统 ffmpeg 不受影响）");
        Ok(())
    }

    /// 流式下载到 `dest`，带百分比进度；返回写入字节数。
    ///
    /// 用 `Response::chunk()` 而不是一次性 `bytes()`：Windows 大包 90MB 全进内存没必要，
    /// 而且流式才能报进度（steamcmd 那种「下载期间无输出」的体验不能再来一次）。
    async fn download_to(app: &AppHandle, src: &Source, dest: &Path) -> Result<u64, String> {
        use tokio::io::AsyncWriteExt;

        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30 * 60))
            .user_agent(USER_AGENT);
        if let Some(p) = crate::download::read_proxy(app) {
            builder = builder.proxy(reqwest::Proxy::all(&p).map_err(|e| format!("代理配置无效: {e}"))?);
        }
        let client = builder.build().map_err(|e| e.to_string())?;
        let mut resp = client
            .get(src.url)
            .send()
            .await
            .map_err(|e| format!("{}: {e}", src.label))?;
        if !resp.status().is_success() {
            return Err(format!("{}: HTTP {}", src.label, resp.status()));
        }
        let total = resp.content_length().unwrap_or(0);
        let mut file = tokio::fs::File::create(dest)
            .await
            .map_err(|e| format!("创建下载文件失败: {e}"))?;
        let mut written = 0u64;
        let mut last_pct = -1.0f64;
        while let Some(chunk) = resp.chunk().await.map_err(|e| format!("{}: {e}", src.label))? {
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("写入下载文件失败: {e}"))?;
            written += chunk.len() as u64;
            if total > 0 {
                let pct = written as f64 * 100.0 / total as f64;
                // 每 1% 报一次：小包不至于刷屏，大包也不会长时间没反应
                if pct - last_pct >= 1.0 {
                    last_pct = pct;
                    emit(
                        app,
                        "download",
                        pct,
                        &format!(
                            "{}：{:.1} / {:.1} MB",
                            src.label,
                            written as f64 / 1048576.0,
                            total as f64 / 1048576.0
                        ),
                    );
                }
            }
        }
        file.flush().await.ok();
        Ok(written)
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

}

// ---------- 其余平台：桩 ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wpem-ffmpeg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn parse_version_reads_first_line() {
        let out = "ffmpeg version 7.1.1-essentials_build-www.gyan.dev Copyright (c) 2000-2024\nbuilt with gcc\n";
        assert_eq!(
            parse_version(out).as_deref(),
            Some("7.1.1-essentials_build-www.gyan.dev")
        );
        // BtbN 这类「N-<build>-<hash>」版本号同样取首段
        assert_eq!(
            parse_version("ffmpeg version N-118219-g0a3f3b4 Copyright (c) 2000-2024").as_deref(),
            Some("N-118219-g0a3f3b4")
        );
        // 非 ffmpeg 输出（例如包装脚本的报错）不能被当成版本号
        assert_eq!(parse_version("command not found"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn find_binary_prefers_bin_directory() {
        let root = tmpdir("binpref");
        // 模拟 GitHub 构建布局：ffmpeg-<ver>/bin/ffmpeg[.exe]
        let bin = root.join("ffmpeg-7.1-essentials_build").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("ffmpeg"), b"x").unwrap();
        std::fs::write(bin.join("ffmpeg.exe"), b"x").unwrap();
        // 同目录的其它可执行文件不该被选中
        std::fs::write(bin.join("ffprobe"), b"x").unwrap();
        assert_eq!(find_binary(&root, "ffmpeg"), Some(bin.join("ffmpeg")));
        assert_eq!(
            find_binary(&root, "ffmpeg.exe"),
            Some(bin.join("ffmpeg.exe"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_binary_falls_back_to_nested_root() {
        let root = tmpdir("rootlevel");
        // 模拟 johnvansickle 静态包：ffmpeg-7.1-amd64-static/ffmpeg（不在 bin/ 下）
        let inner = root.join("ffmpeg-7.1-amd64-static");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("ffmpeg"), b"x").unwrap();
        std::fs::write(inner.join("README.txt"), b"x").unwrap();
        assert_eq!(find_binary(&root, "ffmpeg"), Some(inner.join("ffmpeg")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_binary_none_when_absent() {
        let root = tmpdir("absent");
        std::fs::create_dir_all(root.join("a").join("b")).unwrap();
        assert_eq!(find_binary(&root, "ffmpeg"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 端到端走一遍「解压 + 定位」：造一个与 Windows 上游同构的 zip
    /// （`ffmpeg-<ver>-essentials_build/bin/ffmpeg.exe`），确认能解出来并找准二进制。
    /// 这一段是安装流程里最容易出错的地方（上游包目录结构各不相同），必须能离线验证。
    #[tokio::test]
    async fn extract_zip_then_locate_binary() {
        use std::io::Write;

        let root = tmpdir("extractzip");
        let archive = root.join("ffmpeg.zip");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("ffmpeg-7.1-essentials_build/bin/ffmpeg.exe", opts)
                .unwrap();
            zip.write_all(b"fake-exe").unwrap();
            zip.start_file("ffmpeg-7.1-essentials_build/bin/ffprobe.exe", opts)
                .unwrap();
            zip.write_all(b"fake-probe").unwrap();
            zip.start_file("ffmpeg-7.1-essentials_build/LICENSE", opts)
                .unwrap();
            zip.write_all(b"license").unwrap();
            zip.finish().unwrap();
        }
        let dest = root.join("extract");
        std::fs::create_dir_all(&dest).unwrap();
        extract(&archive, &dest).await.unwrap();
        let got = find_binary(&dest, "ffmpeg.exe").unwrap();
        assert_eq!(
            got,
            dest.join("ffmpeg-7.1-essentials_build")
                .join("bin")
                .join("ffmpeg.exe")
        );
        assert_eq!(std::fs::read(&got).unwrap(), b"fake-exe");
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// macOS 用系统 AVFoundation 抽帧，不需要 ffmpeg：保留同一套 API 形状，
/// 安装直接报错、`supported` 恒为 false（设置页据此隐藏该行）。
#[cfg(not(any(target_os = "windows", target_os = "linux")))]
mod imp {
    use super::*;
    use serde_json::json;
    use tauri::Manager;

    const BIN_NAME: &str = "ffmpeg";

    fn managed_dir(app: &AppHandle) -> Result<PathBuf, String> {
        Ok(app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join(DIR_NAME))
    }

    fn managed_binary(app: &AppHandle) -> Result<PathBuf, String> {
        Ok(managed_dir(app)?.join(BIN_NAME))
    }

    pub fn init(_app: &AppHandle) {}

    pub fn status(app: &AppHandle) -> serde_json::Value {
        json!({
            "supported": false,
            "managed": false,
            "system": false,
            "active": serde_json::Value::Null,
            "version": serde_json::Value::Null,
            "path": serde_json::Value::Null,
            "sizeBytes": 0,
            "dir": managed_dir(app).ok().map(|d| d.display().to_string()),
            "expectedDownloadBytes": 0,
        })
    }

    pub async fn install(_app: AppHandle, _force: bool) -> Result<serde_json::Value, String> {
        Err("当前平台不需要 ffmpeg（macOS 走系统 AVFoundation 抽帧）".into())
    }

    pub fn uninstall(_app: &AppHandle) -> Result<(), String> {
        Ok(())
    }
}
