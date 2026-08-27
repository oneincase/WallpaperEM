//! 下载引擎（T2）：DepotDownloader sidecar + 串行队列 + Steam Guard 交互 + 产物收编
//!
//! 状态机：queued → authenticating → downloading → installing → done | failed
//! 对齐 Web 版 worker.ts：Guard/成功/失败/进度正则、工作目录布局、错误映射。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::oneshot;

use crate::db;
use crate::keychain::get_password;
use crate::secure_store;

pub const APP_ID: &str = "431960";
const GUARD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const PROGRESS_THROTTLE: Duration = Duration::from_millis(1000);

const GUARD_RE: &str = r"(?i)enter the (current )?auth code|two-factor code|steam guard";
const SUCCESS_RE: &str = r"(?i)Downloaded published file|Downloaded depot \d+ of|Depot \d+ - Downloaded \d+ bytes|Total downloaded: .* from \d+ depots";
const PROGRESS_RE: &str = r"(?i)progress:\s*([\d.]+)";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRow {
    pub id: i64,
    pub item_id: String,
    pub title: String,
    pub status: String,
    pub progress: f64,
    pub error_code: Option<String>,
    pub error_msg: Option<String>,
    pub waiting_guard: bool,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

#[derive(Clone)]
pub struct DownloadService {
    db: Arc<Mutex<Connection>>,
    app: AppHandle,
    guard_waiters: Arc<Mutex<HashMap<i64, oneshot::Sender<String>>>>,
    current_task: Arc<Mutex<Option<i64>>>,
    current_child: Arc<tokio::sync::Mutex<Option<Child>>>,
}

impl DownloadService {
    fn new(db: Arc<Mutex<Connection>>, app: AppHandle) -> Self {
        Self {
            db,
            app,
            guard_waiters: Arc::new(Mutex::new(HashMap::new())),
            current_task: Arc::new(Mutex::new(None)),
            current_child: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.app.emit(event, payload);
    }

    // ---------- 工具路径 / 工作目录 ----------

    fn sidecar_path(&self) -> Result<PathBuf, String> {
        // dev：tauri dev 把 externalBin 复制到 target/<profile>/（与可执行同目录）
        if let Ok(exe) = std::env::current_exe() {
            let p = exe.parent().unwrap_or(Path::new(".")).join("depot-downloader");
            if p.exists() {
                return Ok(p);
            }
        }
        // prod：Resources/depot-downloader
        let res = self
            .app
            .path()
            .resource_dir()
            .map_err(|e| e.to_string())?;
        Ok(res.join("depot-downloader"))
    }

    fn data_dir(&self) -> Result<PathBuf, String> {
        self.app.path().app_data_dir().map_err(|e| e.to_string())
    }

    fn workdir(&self) -> Result<PathBuf, String> {
        Ok(self.data_dir()?.join("workdir"))
    }

    fn wallpapers_dir(&self) -> Result<PathBuf, String> {
        Ok(self.data_dir()?.join("wallpapers"))
    }

    fn configdir(&self) -> Result<PathBuf, String> {
        Ok(self.data_dir()?.join("dd-config"))
    }

    // ---------- 队列 ----------

    fn next_queued(&self) -> Result<Option<(i64, String)>, String> {
        let conn = self.db.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT id, item_id FROM downloads WHERE status = 'queued' ORDER BY id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    fn update(&self, id: i64, status: &str, progress: f64, code: Option<&str>, msg: Option<&str>, waiting_guard: bool) {
        let conn = match self.db.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let finished = if matches!(status, "done" | "failed") {
            chrono::Utc::now().timestamp()
        } else {
            0
        };
        let res = conn.execute(
            "UPDATE downloads SET status = ?1, progress = ?2, error_code = ?3, error_msg = ?4,
             waiting_guard = ?5, finished_at = CASE WHEN ?6 > 0 THEN ?6 ELSE finished_at END
             WHERE id = ?7",
            rusqlite::params![status, progress, code, msg, waiting_guard as i32, finished, id],
        );
        if let Err(e) = res {
            tracing::error!("download update failed (task {id}): {e}");
        }
    }

    fn emit_progress(&self, id: i64, status: &str, progress: f64) {
        self.emit(
            "download:progress",
            json!({ "taskId": id, "status": status, "progress": progress }),
        );
    }

    // ---------- 任务执行 ----------

    async fn run_task(&self, task_id: i64, item_id: String) {
        self.update(task_id, "authenticating", 0.0, None, None, false);
        self.emit_progress(task_id, "authenticating", 0.0);

        let bin = match self.sidecar_path() {
            Ok(b) if b.exists() => b,
            _ => {
                self.fail(task_id, "DEPOTDL_NOT_FOUND", "DepotDownloader 未找到（sidecar 缺失）");
                return;
            }
        };

        let (username, password) = match self.get_credentials() {
            Ok(c) => c,
            Err(e) => {
                self.fail(task_id, "NEED_CREDENTIALS", &e);
                return;
            }
        };

        // 清理工作目录（DepotDownloader 直接解压到 -dir 根）
        let workdir = match self.workdir() {
            Ok(w) => w,
            Err(e) => {
                self.fail(task_id, "IO_ERROR", &e);
                return;
            }
        };
        let _ = std::fs::remove_dir_all(&workdir);
        let _ = std::fs::create_dir_all(&workdir);
        let configdir = match self.configdir() {
            Ok(c) => c,
            Err(e) => {
                self.fail(task_id, "IO_ERROR", &e);
                return;
            }
        };
        let _ = std::fs::create_dir_all(&configdir);

        let proxy = {
            let conn = self.db.lock().ok();
            conn.and_then(|c| {
                db::get_setting(&c, "download_proxy").or_else(|| db::get_setting(&c, "steam_proxy"))
            })
        };

        let mut cmd = Command::new(&bin);
        cmd.arg("-app")
            .arg(APP_ID)
            .arg("-pubfile")
            .arg(&item_id)
            .arg("-username")
            .arg(&username)
            .arg("-password")
            .arg(&password)
            .arg("-dir")
            .arg(&workdir)
            .arg("-configdir")
            .arg(&configdir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(p) = &proxy {
            cmd.env("http_proxy", p)
                .env("https_proxy", p)
                .env("HTTP_PROXY", p)
                .env("HTTPS_PROXY", p);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.fail(task_id, "SPAWN_FAILED", &format!("启动下载工具失败: {e}"));
                return;
            }
        };
        let mut stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        *self.current_child.lock().await = Some(child); // 供 cancel 强杀
        let mut stdout_rx = spawn_reader(stdout);
        let mut stderr_rx = spawn_reader(stderr);

        let mut success = false;
        let mut error_msg: Option<String> = None;
        let mut recent: Vec<String> = Vec::new();
        let mut last_progress_at = std::time::Instant::now() - PROGRESS_THROTTLE;
        let mut guard_rx: Option<oneshot::Receiver<String>> = None;
        let mut exit_code: Option<i32> = None;

        loop {
            tokio::select! {
                line = stdout_rx.recv() => {
                    if let Some(l) = line {
                        self.handle_line(&l, task_id, &mut guard_rx, &mut success, &mut error_msg, &mut recent, &mut last_progress_at);
                    }
                }
                line = stderr_rx.recv() => {
                    if let Some(l) = line {
                        self.handle_line(&l, task_id, &mut guard_rx, &mut success, &mut error_msg, &mut recent, &mut last_progress_at);
                    }
                }
                code = async {
                    match guard_rx.as_mut() {
                        Some(rx) => tokio::time::timeout(GUARD_TIMEOUT, rx)
                            .await
                            .ok()
                            .and_then(|r| r.ok()),
                        None => std::future::pending::<Option<String>>().await,
                    }
                }, if guard_rx.is_some() => {
                    if let Some(code) = code {
                        let _ = stdin.write_all(format!("{code}\n").as_bytes()).await;
                        let _ = stdin.flush().await;
                        tracing::info!("guard code submitted for task {task_id}");
                    } else {
                        error_msg = Some("Steam Guard 验证码输入超时".into());
                        let _ = self
                            .current_child
                            .lock()
                            .await
                            .as_mut()
                            .and_then(|c| c.start_kill().ok());
                    }
                    guard_rx = None;
                    self.update(task_id, "authenticating", 0.0, None, None, false);
                }
                st = async {
                    let mut g = self.current_child.lock().await;
                    match g.as_mut() {
                        Some(c) => c.wait().await.ok(),
                        None => None,
                    }
                }, if exit_code.is_none() => {
                    exit_code = st.map(|s| s.code().unwrap_or(-1));
                    tracing::info!("download tool exited: {exit_code:?}");
                }
            }
            if exit_code.is_some() {
                break;
            }
        }
        let _ = stdin.shutdown().await;

        // 清理子进程引用
        *self.current_child.lock().await = None;

        // 成功判定（稳健版）：退出码 0 且 workdir 存在下载产物。
        // 不依赖输出文本匹配 —— DepotDownloader 的最终 "Total downloaded" 行
        // 可能因进度显示/缓冲未进入最近输出，导致旧版误判失败。
        let files_ok = workdir_has_content(&workdir);
        let real_success = exit_code == Some(0) && files_ok && error_msg.is_none();

        if error_msg.is_none() && !real_success {
            if recent.is_empty() {
                error_msg = Some(format!(
                    "下载工具退出码 {exit_code:?}，且工作目录无产物"
                ));
            } else {
                error_msg = Some(format!(
                    "下载工具退出码 {exit_code:?}；最近输出：{}",
                    recent.iter().rev().take(6).cloned().collect::<Vec<_>>().join(" | ")
                ));
            }
        }

        if let Some(err) = error_msg {
            self.fail(task_id, "DOWNLOAD_FAILED", &err);
            return;
        }
        if !real_success {
            self.fail(task_id, "DOWNLOAD_FAILED", "下载未完成");
            return;
        }

        // 安装：收编进壁纸库
        self.update(task_id, "installing", 100.0, None, None, false);
        self.emit_progress(task_id, "installing", 100.0);
        if let Err(e) = self.install(item_id.clone()) {
            self.fail(task_id, "INSTALL_FAILED", &e);
            return;
        }
        self.update(task_id, "done", 100.0, None, None, false);
        self.emit_progress(task_id, "done", 100.0);
        tracing::info!("download done: item {item_id}");
    }

    fn handle_line(
        &self,
        line: &str,
        task_id: i64,
        guard_rx: &mut Option<oneshot::Receiver<String>>,
        success: &mut bool,
        error_msg: &mut Option<String>,
        recent: &mut Vec<String>,
        last_progress_at: &mut std::time::Instant,
    ) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        if !progress_re_match(line) {
            recent.push(line.to_string());
            if recent.len() > 25 {
                recent.remove(0);
            }
        }

        if let Some(re) = regex::Regex::new(SUCCESS_RE).ok() {
            if re.is_match(line) {
                *success = true;
            }
        }
        if let Some(re) = regex::Regex::new(GUARD_RE).ok() {
            if re.is_match(line) && guard_rx.is_none() {
                let (tx, rx) = oneshot::channel();
                self.guard_waiters.lock().unwrap().insert(task_id, tx);
                *guard_rx = Some(rx);
                self.update(task_id, "authenticating", 0.0, None, None, true);
                self.emit("download:guard-required", json!({ "taskId": task_id }));
                tracing::info!("Steam Guard 验证码请求（task {task_id}）");
            }
        }
        if error_msg.is_none() {
            if let Some(msg) = parse_failure(line) {
                *error_msg = Some(msg);
            }
        }
        // 进度：兼容 "Progress: 12.3%" 与 DepotDownloader 逐文件 "99.66% /path/file" 两种格式
        let mut pct: Option<f64> = None;
        if let Some(re) = regex::Regex::new(PROGRESS_RE).ok() {
            if let Some(caps) = re.captures(line) {
                pct = caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok());
            }
        }
        if pct.is_none() {
            // 逐文件百分比：" 99.66% /path"
            if let Some(re) = regex::Regex::new(r"^\s*(\d+\.?\d*)%\s+/").ok() {
                if let Some(caps) = re.captures(line) {
                    pct = caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok());
                }
            }
        }
        if let Some(p) = pct {
            if last_progress_at.elapsed() >= PROGRESS_THROTTLE {
                *last_progress_at = std::time::Instant::now();
                let cur = {
                    let conn = self.db.lock().ok();
                    conn.and_then(|c| {
                        c.query_row(
                            "SELECT progress FROM downloads WHERE id = ?1",
                            [task_id],
                            |r| r.get::<_, f64>(0),
                        )
                        .ok()
                    })
                    .unwrap_or(0.0)
                };
                let pct = p.min(99.0).max(cur);
                self.update(task_id, "downloading", pct, None, None, false);
                self.emit_progress(task_id, "downloading", pct);
            }
        }
    }

    fn get_credentials(&self) -> Result<(String, String), String> {
        let conn = self.db.lock().map_err(|e| e.to_string())?;
        let username = db::get_setting(&conn, "download_username")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "请先在设置页配置下载账号（需拥有 Wallpaper Engine）".to_string())?;
        let dir = self.data_dir()?;
        // 首选：本地加密存储（避免每次 Keychain 授权）
        if let Some(pw) = secure_store::load(&username, &dir).ok().flatten() {
            return Ok((username, pw));
        }
        // 回退：旧的 Keychain 凭据（历史账号），并迁移到本地存储，后续不再弹授权
        let pw = get_password(&username)
            .map_err(|_| "读取密码失败，请重新配置下载账号".to_string())?;
        let _ = secure_store::save(&username, &pw, &dir);
        Ok((username, pw))
    }

    fn fail(&self, id: i64, code: &str, msg: &str) {
        tracing::error!("download task {id} failed: {code} {msg}");
        self.guard_waiters.lock().unwrap().remove(&id);
        self.update(id, "failed", 0.0, Some(code), Some(msg), false);
        self.emit_progress(id, "failed", 0.0);
    }

    /// 收编：把下载产物移入壁纸库，解析 project.json 登记类型
    fn install(&self, item_id: String) -> Result<(), String> {
        // 该 item 若是某壁纸的依赖（target_dir 记录了主壁纸目录），下载完成后需合并 scene.pkg
        let dependency_targets: Vec<String> = {
            let conn = self.db.lock().map_err(|e| e.to_string())?;
            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT target_dir FROM downloads
                     WHERE item_id = ?1 AND target_dir IS NOT NULL AND target_dir != ''",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([&item_id], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r.map_err(|e| e.to_string())?);
            }
            v
        };

        let workdir = self.workdir()?;
        let steamcmd_path = workdir.join("steamapps/workshop/content").join(APP_ID).join(&item_id);
        let mut src: Option<PathBuf> = None;
        if steamcmd_path.is_dir() {
            src = Some(steamcmd_path);
        } else if workdir.is_dir() {
            let entries = std::fs::read_dir(&workdir).map_err(|e| e.to_string())?;
            let has_content = entries.into_iter().next().is_some();
            if has_content {
                src = Some(workdir.clone());
            }
        }
        let Some(src) = src else {
            return Err("安装失败：未找到下载产物".into());
        };

        let dest = self.wallpapers_dir()?.join(&item_id);
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        let mut file_count = 0usize;
        let mut size_bytes = 0u64;
        for entry in std::fs::read_dir(&src).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            // 根布局时跳过遗留 steamapps 目录
            if src == workdir && name == "steamapps" {
                continue;
            }
            let from = entry.path();
            let to = dest.join(&name);
            copy_recursive(&from, &to, &mut file_count, &mut size_bytes)?;
        }

        // 本 item 是某壁纸的依赖：把 scene.pkg 合并到主壁纸目录
        if !dependency_targets.is_empty() {
            for target in &dependency_targets {
                let target = std::path::PathBuf::from(target);
                let pkg_src = if dest.join("scene.pkg").is_file() {
                    Some(dest.join("scene.pkg"))
                } else if dest.join("scenes/scene.pkg").is_file() {
                    Some(dest.join("scenes/scene.pkg"))
                } else {
                    None
                };
                if let Some(src) = pkg_src {
                    let dst = target.join("scene.pkg");
                    let _ = std::fs::create_dir_all(&target);
                    if std::fs::copy(&src, &dst).is_ok() {
                        tracing::info!("merged scene.pkg from dependency {item_id} -> {target:?}");
                    }
                }
                // 清理依赖任务标记（已完成合并）
                let conn = self.db.lock().map_err(|e| e.to_string())?;
                let t_str = target.to_string_lossy().to_string();
                let _ = conn.execute(
                    "UPDATE downloads SET target_dir = NULL WHERE item_id = ?1 AND target_dir = ?2",
                    rusqlite::params![item_id, t_str],
                );
            }
        }

        // 解析 project.json 登记类型
        let project_path = dest.join("project.json");
        let mut wtype = "unknown".to_string();
        let mut title = item_id.clone();
        if let Ok(text) = std::fs::read_to_string(&project_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(t) = v.get("type").and_then(|t| t.as_str()) {
                    wtype = crate::steam::details::infer_type_from_tags(&[t.to_string()]);
                }
                if let Some(t) = v.get("title").and_then(|t| t.as_str()) {
                    title = t.to_string();
                }
            }
        }
        // project.json 无 type 或推断不出时：回退工坊元数据（workshop_items 已存正确类型）
        if wtype == "unknown" {
            if let Ok(Some(meta)) = crate::db::find_workshop_item(&self.db, &item_id) {
                if meta.r#type != "unknown" {
                    wtype = meta.r#type.clone();
                }
            }
        }
        let conn = self.db.lock().map_err(|e| e.to_string())?;
        let _ = conn.execute(
            "INSERT INTO library_items(item_id, title, type, size_bytes, file_count)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(item_id) DO UPDATE SET title = ?2, type = ?3, size_bytes = ?4, file_count = ?5",
            rusqlite::params![item_id, title, wtype, size_bytes as i64, file_count as i64],
        );

        // 场景壁纸缺 scene.pkg 但声明了 dependency：自动把依赖加入下载队列（合并 scene.pkg）
        if wtype == "scene" || !dest.join("scene.pkg").is_file() {
            let dep = std::fs::read_to_string(&project_path)
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|v| v.get("dependency").and_then(|d| d.as_str()).map(|s| s.to_string()))
                .filter(|d| !d.is_empty() && d != &item_id);
            if let Some(dep_id) = dep {
                let has_pkg = dest.join("scene.pkg").is_file()
                    || dest.join("scenes/scene.pkg").is_file();
                let dep_already = self.wallpapers_dir().ok().map(|dir| {
                    dir.join(&dep_id).join("scene.pkg").is_file()
                        || dir.join(&dep_id).join("scenes/scene.pkg").is_file()
                }) == Some(true);
                if !has_pkg && !dep_already {
                    // 依赖尚未下载：加入队列，target_dir 指向主壁纸目录（合并用）
                    let queued: bool = conn
                        .query_row(
                            "SELECT COUNT(*) FROM downloads WHERE item_id = ?1 AND status IN ('queued','authenticating','downloading','installing')",
                            [&dep_id],
                            |r| r.get::<_, i64>(0),
                        )
                        .map(|n| n > 0)
                        .unwrap_or(false);
                    if !queued {
                        let dest_s = dest.to_string_lossy().to_string();
                        let _ = conn.execute(
                            "INSERT INTO downloads(item_id, status, progress, target_dir, created_at)
                             VALUES (?1, 'queued', 0, ?2, unixepoch())",
                            rusqlite::params![dep_id, dest_s],
                        );
                        tracing::info!(
                            "item {item_id} needs dependency {dep_id} (missing scene.pkg); queued dependency download"
                        );
                    }
                }
            }
        }
        drop(conn);
        tracing::info!("installed item {item_id}: type={wtype}, files={file_count}, size={size_bytes}");
        Ok(())
    }

    fn submit_guard(&self, task_id: i64, code: String) -> bool {
        let sender = self.guard_waiters.lock().unwrap().remove(&task_id);
        match sender {
            Some(tx) => tx.send(code.trim().to_string()).is_ok(),
            None => false,
        }
    }

    fn cancel(&self, task_id: i64) -> Result<(), String> {
        let is_current = *self.current_task.lock().unwrap() == Some(task_id);
        if is_current {
            if let Ok(mut guard) = self.current_child.try_lock() {
                if let Some(c) = guard.as_mut() {
                    let _ = c.start_kill();
                }
            }
            return Ok(());
        }
        // 排队中：直接标记取消
        let conn = self.db.lock().map_err(|e| e.to_string())?;
        let rows = conn
            .execute(
                "UPDATE downloads SET status='failed', error_code='CANCELLED', error_msg='已取消',
                 finished_at = unixepoch() WHERE id = ?1 AND status = 'queued'",
                [task_id],
            )
            .map_err(|e| e.to_string())?;
        if rows == 0 {
            return Err("任务不存在或已开始".into());
        }
        Ok(())
    }
}

fn progress_re_match(line: &str) -> bool {
    regex::Regex::new(PROGRESS_RE)
        .map(|re| re.is_match(line))
        .unwrap_or(false)
}

/// 工作目录是否存在下载产物（排除 .DepotDownloader 元数据目录）
fn workdir_has_content(workdir: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(workdir) else {
        return false;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "steamapps" {
            continue;
        }
        return true;
    }
    false
}

/// 输出行 → 结构化失败原因（对齐 Web 版 parseFailure）
fn parse_failure(line: &str) -> Option<String> {
    let l = line;
    if l.to_lowercase().contains("invalid password") {
        return Some("账号或密码错误".into());
    }
    if regex::Regex::new(r"(?i)no license|no subscription|doesn't own|not licensed")
        .map(|re| re.is_match(l))
        .unwrap_or(false)
    {
        return Some("该账号未拥有 Wallpaper Engine".into());
    }
    if regex::Regex::new(r"(?i)rate limit|too many login attempts")
        .map(|re| re.is_match(l))
        .unwrap_or(false)
    {
        return Some("登录过于频繁，请稍后再试".into());
    }
    if l.to_lowercase().contains("multiple logins") {
        return Some("该账号在其他设备登录，请稍后再试".into());
    }
    if regex::Regex::new(r"(?i)login failure|account logon denied|login denied")
        .map(|re| re.is_match(l))
        .unwrap_or(false)
    {
        return Some(format!("Steam 登录被拒绝：{}", l.chars().take(120).collect::<String>()));
    }
    if l.to_lowercase().contains("account has been locked") {
        return Some(format!("Steam 账号已被锁定：{}", l.chars().take(120).collect::<String>()));
    }
    if regex::Regex::new(r"(?i)unable to connect|failed to connect")
        .map(|re| re.is_match(l))
        .unwrap_or(false)
    {
        return Some(format!("无法连接 Steam 服务器：{}", l.chars().take(120).collect::<String>()));
    }
    None
}

/// 流式读取子进程输出行，转发到 mpsc
fn spawn_reader(
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
) -> tokio::sync::mpsc::Receiver<String> {
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(256);
    tauri::async_runtime::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(line).await.is_err() {
                break;
            }
        }
    });
    rx
}

fn copy_recursive(from: &Path, to: &Path, file_count: &mut usize, size_bytes: &mut u64) -> Result<(), String> {
    if from.is_dir() {
        std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
        for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()), file_count, size_bytes)?;
        }
    } else {
        std::fs::copy(from, to).map_err(|e| e.to_string())?;
        *file_count += 1;
        *size_bytes += from.metadata().map(|m| m.len()).unwrap_or(0);
    }
    Ok(())
}

// ---------- 初始化与 Tauri 命令 ----------

pub fn init(app: &AppHandle) -> Result<(), String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let svc = Arc::new(DownloadService::new(db.inner().clone(), app.clone()));
    app.manage(svc.clone());

    // 重启恢复：中断任务标记失败
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let _ = conn.execute(
            "UPDATE downloads SET status='failed', error_code='RESTARTED', error_msg='应用重启，任务中断',
             waiting_guard=0, finished_at = unixepoch()
             WHERE status IN ('queued','authenticating','downloading','installing')",
            [],
        );
    }

    // 串行 worker
    tauri::async_runtime::spawn(async move {
        loop {
            let task = match svc.next_queued() {
                Ok(Some(t)) => t,
                _ => {
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                    continue;
                }
            };
            *svc.current_task.lock().unwrap() = Some(task.0);
            svc.run_task(task.0, task.1).await;
            *svc.current_task.lock().unwrap() = None;
        }
    });
    Ok(())
}

#[tauri::command]
pub fn download_tool_status(app: AppHandle) -> serde_json::Value {
    let svc = DownloadService::new(
        app.state::<Arc<Mutex<Connection>>>().inner().clone(),
        app.clone(),
    );
    match svc.sidecar_path() {
        Ok(p) if p.exists() => json!({
            "installed": true,
            "path": p.display().to_string(),
            "sizeMB": (p.metadata().map(|m| m.len() / 1024 / 1024).unwrap_or(0)),
            "version": "3.4.0",
        }),
        _ => json!({ "installed": false }),
    }
}

#[tauri::command]
pub fn download_credentials_set(
    app: AppHandle,
    username: String,
    password: String,
) -> Result<serde_json::Value, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    // 首选本地加密存储（不再写 Keychain；旧 Keychain 逻辑保留在 keychain.rs）
    secure_store::save(&username, &password, &dir).map_err(|e| format!("凭据写入失败: {e}"))?;
    // 清理旧的 Keychain 凭据，避免残留（删掉失败忽略）
    let _ = crate::keychain::delete_password(&username);
    let conn = db.lock().map_err(|e| e.to_string())?;
    db::set_setting(&conn, "download_username", &username)?;
    Ok(json!({ "ok": true, "username": username }))
}

#[tauri::command]
pub fn download_credentials_status(app: AppHandle) -> Result<serde_json::Value, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let username = db::get_setting(&conn, "download_username");
    let dir = app.path().app_data_dir().ok();
    let has_pw = match (&username, &dir) {
        (Some(u), Some(d)) => secure_store::has(u, d),
        _ => false,
    };
    Ok(json!({ "configured": username.is_some() && has_pw, "username": username }))
}

#[tauri::command]
pub fn download_enqueue(
    app: AppHandle,
    item_id: String,
) -> Result<i64, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM downloads WHERE item_id = ?1 AND status IN ('queued','authenticating','downloading','installing')",
            [&item_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .map_err(|e| e.to_string())?;
    if exists {
        return Err("该壁纸已在下载队列中".into());
    }
    conn.execute(
        "INSERT INTO downloads(item_id, status, progress, created_at) VALUES (?1, 'queued', 0, unixepoch())",
        [&item_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

#[tauri::command]
pub fn download_list(app: AppHandle) -> Result<Vec<DownloadRow>, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT d.id, d.item_id, COALESCE(w.title, d.item_id), d.status, d.progress,
                    d.error_code, d.error_msg, d.waiting_guard, d.created_at, d.started_at, d.finished_at
             FROM downloads d LEFT JOIN workshop_items w ON w.id = d.item_id
             ORDER BY d.id DESC LIMIT 100",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(DownloadRow {
                id: r.get(0)?,
                item_id: r.get(1)?,
                title: r.get(2)?,
                status: r.get(3)?,
                progress: r.get(4)?,
                error_code: r.get(5)?,
                error_msg: r.get(6)?,
                waiting_guard: r.get(7)?,
                created_at: r.get(8)?,
                started_at: r.get(9)?,
                finished_at: r.get(10)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn download_cancel(app: AppHandle, id: i64) -> Result<bool, String> {
    let svc = app.state::<Arc<DownloadService>>();
    svc.cancel(id)?;
    Ok(true)
}

#[tauri::command]
pub fn download_retry(app: AppHandle, id: i64) -> Result<bool, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE downloads SET status='queued', error_code=NULL, error_msg=NULL, progress=0 WHERE id = ?1",
        [id],
    )
    .map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command]
pub fn download_submit_guard(app: AppHandle, id: i64, code: String) -> Result<bool, String> {
    let svc = app.state::<Arc<DownloadService>>();
    Ok(svc.submit_guard(id, code))
}

/// 移除下载任务（仅限已结束 done/failed 或未开始的 queued；进行中需先取消）
#[tauri::command]
pub fn download_remove(app: AppHandle, id: i64) -> Result<bool, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let rows = conn
        .execute(
            "DELETE FROM downloads WHERE id = ?1 AND status IN ('done','failed','queued')",
            [id],
        )
        .map_err(|e| e.to_string())?;
    if rows == 0 {
        return Err("任务进行中，请先取消再移除".into());
    }
    Ok(true)
}

/// 清空所有已结束任务（done/failed）
#[tauri::command]
pub fn download_clear_finished(app: AppHandle) -> Result<i64, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let n = conn
        .execute("DELETE FROM downloads WHERE status IN ('done','failed')", [])
        .map_err(|e| e.to_string())?;
    Ok(n as i64)
}
