//! 下载引擎（T2）：串行队列 + Steam Guard 交互 + 产物收编
//!
//! 状态机：queued → authenticating → downloading → installing → done | failed
//!
//! 下载工具是 Valve 官方 steamcmd（运行时安装到 app data，见 `steamcmd_install.rs`）。
//! 与它打交道的细节（命令行、输出正则、PTY、产物布局、收尾方式）收敛在 `backend.rs`。

pub mod backend;
pub mod pty;
pub mod steamcmd_install;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::oneshot;

use crate::db;
use crate::keychain::get_password;
use crate::secure_store;
use backend::{Credentials, GuardKind, SteamCmd};

pub const APP_ID: &str = "431960";
const GUARD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// steamcmd 无进度输出，用产物目录大小估算进度的轮询间隔
const SIZE_POLL_INTERVAL: Duration = Duration::from_millis(1000);
/// 密码登录开始后，超过该时长仍未成功就推测在等待手机 App 确认。
///
/// 新版 steamcmd 等待手机确认时不打印任何提示（console_log.txt 实测：
/// "Logging in user..." 之后静默 27 秒直到用户确认），无法靠输出文案检测，
/// 只能用「密码登录 + 长时间未完成」推测。缓存登录通常 4 秒内完成，8 秒不会误伤。
const MOBILE_HINT_DELAY: Duration = Duration::from_secs(8);

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
    /// 依赖补拉任务（主壁纸缺依赖时自动入队，target_dir 记录合并目标）
    pub dependency: bool,
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

    // ---------- 后端选择 ----------

    /// 当前配置的下载后端（默认 steamcmd）
    /// 构造 steamcmd 句柄。
    fn downloader(&self) -> Result<SteamCmd, String> {
        Ok(SteamCmd {
            script: steamcmd_install::script_path(&self.app)?,
            home: steamcmd_install::home_dir(&self.app)?,
        })
    }

    // ---------- 工具路径 / 工作目录 ----------

    fn data_dir(&self) -> Result<PathBuf, String> {
        self.app.path().app_data_dir().map_err(|e| e.to_string())
    }

    fn workdir(&self) -> Result<PathBuf, String> {
        Ok(self.data_dir()?.join("workdir"))
    }

    fn wallpapers_dir(&self) -> Result<PathBuf, String> {
        Ok(self.data_dir()?.join("wallpapers"))
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

    fn update(
        &self,
        id: i64,
        status: &str,
        progress: f64,
        code: Option<&str>,
        msg: Option<&str>,
        waiting_guard: bool,
    ) {
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
            rusqlite::params![
                status,
                progress,
                code,
                msg,
                waiting_guard as i32,
                finished,
                id
            ],
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
        self.mark_started(task_id);

        let dl = match self.downloader() {
            Ok(d) => d,
            Err(e) => {
                self.fail(task_id, "IO_ERROR", &e);
                return;
            }
        };
        if !dl.is_installed() {
            self.fail(
                task_id,
                "STEAMCMD_NOT_FOUND",
                "steamcmd 尚未安装，请到「设置 → 账号」点击安装",
            );
            return;
        }
        let bin = dl.script.clone();

        // 凭据：有持久化登录态时只需账号名；否则需完整账号密码
        // （首次登录后 steamcmd 会把登录态缓存到被重定向的 HOME 下）。
        let cred = match self.resolve_credentials() {
            Ok((username, password, has_token)) => Credentials {
                username,
                password,
                has_token,
            },
            Err(e) => {
                self.fail(task_id, "NEED_CREDENTIALS", &e);
                return;
            }
        };

        let workdir = match self.workdir() {
            Ok(w) => w,
            Err(e) => {
                self.fail(task_id, "IO_ERROR", &e);
                return;
            }
        };
        let _ = std::fs::remove_dir_all(&workdir);
        if let Err(e) = std::fs::create_dir_all(&workdir) {
            self.fail(task_id, "IO_ERROR", &format!("创建工作目录失败: {e}"));
            return;
        }
        // steamcmd 要求 force_install_dir 是绝对路径，否则内容会静默落到 $HOME/Steam
        let workdir = std::fs::canonicalize(&workdir).unwrap_or(workdir);
        if let Ok(h) = steamcmd_install::home_dir(&self.app) {
            let _ = std::fs::create_dir_all(h);
        }

        let proxy = {
            let conn = self.db.lock().ok();
            conn.and_then(|c| {
                db::get_setting(&c, "download_proxy").or_else(|| db::get_setting(&c, "steam_proxy"))
            })
        };

        // 登录验证任务：只 +login +quit，不下载任何内容
        let is_verify = item_id == backend::VERIFY_ITEM_ID;
        let mut cmd = Command::new(&bin);
        if is_verify {
            cmd.args(dl.build_login_args(&workdir, &cred));
        } else {
            cmd.args(dl.build_args(&item_id, &workdir, &cred));
        }
        for (k, v) in dl.extra_env() {
            cmd.env(k, v);
        }
        if let Some(p) = &proxy {
            cmd.env("http_proxy", p)
                .env("https_proxy", p)
                .env("HTTP_PROXY", p)
                .env("HTTPS_PROXY", p);
        }

        // 交互通道：*nix 上 steamcmd 的 stdin 必须是 TTY（管道会让它直接
        // "cannot read from the console" 退出）；Windows 走匿名管道（详见 pty.rs）。
        let (mut writer, mut out_rx) = {
            let mut pty = match pty::Pty::open() {
                Ok(p) => p,
                Err(e) => {
                    self.fail(task_id, "SPAWN_FAILED", &format!("分配交互通道失败: {e}"));
                    return;
                }
            };
            pty.attach(&mut cmd);
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    self.fail(task_id, "SPAWN_FAILED", &format!("启动下载工具失败: {e}"));
                    return;
                }
            };
            // 交互通道必须在 spawn 之后取：Windows 上管道句柄来自 Child 本身
            let (writer, rx) = match pty.channel(&mut child, backend::is_prompt).await {
                Ok(c) => c,
                Err(e) => {
                    let _ = child.start_kill();
                    self.fail(task_id, "SPAWN_FAILED", &format!("建立交互通道失败: {e}"));
                    return;
                }
            };
            *self.current_child.lock().await = Some(child);
            (writer, rx)
        };

        // steamcmd 下载期间零进度输出：轮询产物目录大小估算进度。
        // 登录验证任务没有产物，用 -1 的不确定态进度即可。
        // 注意状态必须保持 authenticating：误报 downloading 会让前端把验证任务
        // 显示成「下载中」，还会触发「登录已推进」逻辑提前收起手机确认弹窗。
        let progress_poll = if is_verify {
            self.emit_progress(task_id, "authenticating", -1.0);
            tauri::async_runtime::spawn(std::future::pending::<()>())
        } else {
            self.spawn_size_poller(
                task_id,
                dl.artifact_dir(&workdir, &item_id),
                item_id.clone(),
            )
        };

        let mut st = TaskState {
            success: false,
            error_msg: None,
            recent: Vec::new(),
            guard_rx: None,
            exit_code: None,
            mobile_confirm_notified: Arc::new(AtomicBool::new(false)),
            mobile_hint_armed: false,
            mobile_hint_cancel: Arc::new(AtomicBool::new(false)),
        };
        let watchdog = backend::WATCHDOG;
        let task_deadline = tokio::time::sleep(watchdog);
        tokio::pin!(task_deadline);

        loop {
            tokio::select! {
                ev = out_rx.recv() => {
                    if let Some(ev) = ev {
                        task_deadline.as_mut().reset(tokio::time::Instant::now() + watchdog);
                        self.handle_event(ev, task_id, &dl, &mut st, is_verify);
                    }
                }
                code = async {
                    match st.guard_rx.as_mut() {
                        Some(rx) => tokio::time::timeout(GUARD_TIMEOUT, rx)
                            .await
                            .ok()
                            .and_then(|r| r.ok()),
                        None => std::future::pending::<Option<String>>().await,
                    }
                }, if st.guard_rx.is_some() => {
                    if let Some(code) = code {
                        // PTY master 是双工的：验证码直接写回同一个句柄
                        let _ = writer.write_all(format!("{code}\n").as_bytes()).await;
                        let _ = writer.flush().await;
                        tracing::info!("guard code submitted for task {task_id}");
                    } else {
                        st.error_msg = Some("Steam Guard 验证码输入超时".into());
                        self.kill_current().await;
                    }
                    st.guard_rx = None;
                    self.update(task_id, "authenticating", 0.0, None, None, false);
                }
                code = async {
                    let mut g = self.current_child.lock().await;
                    match g.as_mut() {
                        Some(c) => c.wait().await.ok(),
                        None => None,
                    }
                }, if st.exit_code.is_none() => {
                    st.exit_code = code.map(|s| s.code().unwrap_or(-1));
                    tracing::info!("download tool exited: {:?}", st.exit_code);
                }
                _ = &mut task_deadline => {
                    tracing::warn!("download task {task_id} timed out after {:?}", watchdog);
                    st.error_msg = Some(format!(
                        "下载超时（{} 分钟无输出），已中止",
                        watchdog.as_secs() / 60
                    ));
                    self.kill_current().await;
                    st.exit_code = Some(-2);
                    break;
                }
            }
            // steamcmd 在 macOS 上下载完成后常卡在 Steam API 拆卸而不退出，
            // 匹配到成功行就主动收尾，不能等退出码。
            if st.success {
                tracing::info!("task {task_id}: 成功标志已出现，主动结束 steamcmd");
                self.kill_current().await;
                st.exit_code = Some(0);
                break;
            }
            if st.exit_code.is_some() {
                break;
            }
        }
        progress_poll.abort();
        // 任务结束，解除「等待手机确认」推测定时器（若尚未触发）
        st.mobile_hint_cancel.store(true, Ordering::SeqCst);
        *self.current_child.lock().await = None;

        // 成功判定：以「成功行 + 产物真实存在」为准（登录验证任务无产物，只看成功行）。
        // 退出码不可信 —— macOS 上 steamcmd 下载成功后常卡在拆卸阶段被我们主动杀掉。
        let real_success = if is_verify {
            st.success && st.error_msg.is_none()
        } else {
            let artifact = dl.artifact_dir(&workdir, &item_id);
            let files_ok = artifact.is_dir() && backend::dir_size(&artifact) > 0;
            st.success && files_ok && st.error_msg.is_none()
        };

        if st.error_msg.is_none() && !real_success {
            if st.recent.is_empty() {
                st.error_msg = Some(format!(
                    "下载工具退出码 {:?}，且工作目录无产物",
                    st.exit_code
                ));
            } else {
                st.error_msg = Some(format!(
                    "下载工具退出码 {:?}；最近输出：{}",
                    st.exit_code,
                    st.recent
                        .iter()
                        .rev()
                        .take(6)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                ));
            }
        }

        if let Some(err) = st.error_msg {
            self.fail(task_id, "DOWNLOAD_FAILED", &err);
            return;
        }
        if !real_success {
            self.fail(
                task_id,
                "DOWNLOAD_FAILED",
                if is_verify { "登录验证未完成" } else { "下载未完成" },
            );
            return;
        }

        // 登录验证任务：登录成功即完成，无需安装
        if is_verify {
            if let Ok(conn) = self.db.lock() {
                let _ = db::set_setting(&conn, "download_has_token", "true");
                let _ = db::set_setting(&conn, steamcmd_install::SETTING_LOGGED_IN, "true");
            }
            self.update(task_id, "done", 100.0, None, None, false);
            self.emit_progress(task_id, "done", 100.0);
            tracing::info!("login verify done (task {task_id})");
            return;
        }

        // 安装：收编进壁纸库（阻塞式大文件拷贝，移到 spawn_blocking，避免占满 async runtime 工作线程
        // 导致内容服务器/UI 事件响应变慢而白屏）
        self.update(task_id, "installing", 100.0, None, None, false);
        self.emit_progress(task_id, "installing", 100.0);
        let svc = self.clone();
        let item = item_id.clone();
        let install_res = tokio::task::spawn_blocking(move || svc.install(item)).await;
        match install_res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                self.fail(task_id, "INSTALL_FAILED", &e);
                return;
            }
            Err(e) => {
                self.fail(task_id, "INSTALL_FAILED", &format!("安装任务异常: {e}"));
                return;
            }
        }
        // 下载成功：登录令牌已持久化，置位标记，后续下载省略密码走令牌登录。
        // 注意 backend_kind() 内部也会 lock db，必须在取锁前先算好，否则自锁死。
        if let Ok(conn) = self.db.lock() {
            let _ = db::set_setting(&conn, "download_has_token", "true");
            let _ = db::set_setting(&conn, steamcmd_install::SETTING_LOGGED_IN, "true");
        }
        self.update(task_id, "done", 100.0, None, None, false);
        self.emit_progress(task_id, "done", 100.0);
        tracing::info!("download done: item {item_id}");
    }

    /// 记录任务开始时间（原实现从未写入该列，前端拿到的 startedAt 恒为 null）
    fn mark_started(&self, task_id: i64) {
        if let Ok(conn) = self.db.lock() {
            let _ = conn.execute(
                "UPDATE downloads SET started_at = unixepoch() WHERE id = ?1",
                [task_id],
            );
        }
    }

    async fn kill_current(&self) {
        if let Some(c) = self.current_child.lock().await.as_mut() {
            let _ = c.start_kill();
        }
    }

    /// 后台轮询产物目录大小估算进度（steamcmd 下载期间零进度输出）。
    ///
    /// 总大小取自工坊元数据的 `file_size`；取不到时发 -1 让前端显示不确定态进度条。
    fn spawn_size_poller(
        &self,
        task_id: i64,
        artifact: PathBuf,
        item_id: String,
    ) -> tauri::async_runtime::JoinHandle<()> {
        let svc = self.clone();
        tauri::async_runtime::spawn(async move {
            let total = svc.lookup_file_size(&item_id).await;
            loop {
                tokio::time::sleep(SIZE_POLL_INTERVAL).await;
                let downloaded = backend::dir_size(&artifact);
                let pct = match total {
                    Some(t) if t > 0 => {
                        let raw = downloaded as f64 / t as f64 * 100.0;
                        raw.min(99.0)
                    }
                    // 总大小未知：用 -1 表达「进行中但比例未知」
                    _ => -1.0,
                };
                if pct < 0.0 {
                    svc.update(task_id, "downloading", -1.0, None, None, false);
                    svc.emit_progress(task_id, "downloading", -1.0);
                    continue;
                }
                let cur = svc.current_progress(task_id);
                // 单调不减，避免目录被重写时进度回退
                let pct = pct.max(cur.max(0.0));
                svc.update(task_id, "downloading", pct, None, None, false);
                svc.emit_progress(task_id, "downloading", pct);
            }
        })
    }

    /// 取条目的字节大小：优先本地工坊元数据缓存，缺失时现拉一次详情接口。
    async fn lookup_file_size(&self, item_id: &str) -> Option<i64> {
        if let Ok(Some(item)) = db::find_workshop_item(&self.db, item_id) {
            if let Some(s) = item.file_size.filter(|v| *v > 0) {
                return Some(s);
            }
        }
        let client = self.app.try_state::<crate::steam::SteamClient>()?;
        let items = crate::steam::details::get_item_details(&client, &[item_id.to_string()])
            .await
            .ok()?;
        let item = items.into_iter().next()?;
        if let Ok(conn) = self.db.lock() {
            let _ = db::upsert_workshop_item(&conn, &item);
        }
        item.file_size.filter(|v| *v > 0)
    }

    fn current_progress(&self, task_id: i64) -> f64 {
        self.db
            .lock()
            .ok()
            .and_then(|c| {
                c.query_row(
                    "SELECT progress FROM downloads WHERE id = ?1",
                    [task_id],
                    |r| r.get::<_, f64>(0),
                )
                .ok()
            })
            .unwrap_or(0.0)
    }

    /// 处理子进程一个输出事件（完整行 / 未换行的输入提示）
    fn handle_event(
        &self,
        ev: pty::OutEvent,
        task_id: i64,
        dl: &SteamCmd,
        st: &mut TaskState,
        is_verify: bool,
    ) {
        let text = match &ev {
            pty::OutEvent::Line(l) => l.clone(),
            pty::OutEvent::Prompt(p) => p.clone(),
        };
        let line = text.trim();
        if line.is_empty() {
            return;
        }

        // 密码登录开始：启动「静默等待手机确认」推测定时器。
        // Prompt（"Logging in user..." 的无换行尾部）和 Line 都要检查。
        if dl.match_login_start(line) {
            self.arm_mobile_hint(task_id, st);
        }

        // 提示类事件：仅用于尽早弹出输入框，不进日志缓冲
        if matches!(ev, pty::OutEvent::Prompt(_)) {
            self.request_guard(task_id, dl, line, st);
            return;
        }

        st.recent.push(line.to_string());
        if st.recent.len() > 25 {
            st.recent.remove(0);
        }
        // 登录成功行：对验证任务是完成标志；对普通下载任务至少说明登录阶段
        // 已结束，必须解除手机确认推测（match_success 要等下载结束才出现，
        // 靠它解除的话，用户 8 秒内确认完、下载刚开始时会误弹提示）。
        if dl.match_login_success(line) {
            st.mobile_hint_cancel.store(true, Ordering::SeqCst);
            if is_verify {
                st.success = true;
            }
        }
        if dl.match_success(line) {
            st.success = true;
            st.mobile_hint_cancel.store(true, Ordering::SeqCst);
        }
        self.request_guard(task_id, dl, line, st);
        if st.error_msg.is_none() {
            if let Some(msg) = dl.parse_failure(line) {
                st.error_msg = Some(msg);
            }
        }
    }

    /// 识别到需要用户输入验证码时，建立一次性通道并通知前端。
    /// 手机确认类提示只做日志，不弹输入框。
    fn request_guard(&self, task_id: i64, dl: &SteamCmd, line: &str, st: &mut TaskState) {
        let Some(kind) = dl.match_guard_prompt(line) else {
            // 诊断：含有 confirm 字样却没匹配上的行打出来，方便发现 steamcmd
            // 又改了提示文案（之前已经改过一次：新版干脆不打印了）
            if line.to_ascii_lowercase().contains("confirm") {
                tracing::warn!("task {task_id}: 疑似确认提示但未匹配: {line}");
            }
            return;
        };
        tracing::info!("task {task_id}: 检测到交互请求 {kind:?}: {line}");
        match kind {
            GuardKind::MobileConfirm => {
                // 只提示一次：steamcmd 在等待期间会反复刷这句提示。
                // 与静默推测定时器抢同一个标志，谁先谁发。
                self.notify_mobile_confirm(task_id, st, "steamcmd 输出了确认提示");
            }
            GuardKind::Password => {
                // 令牌失效时后端会重新索要密码。这里不弹框，直接标记失败让用户重新登录，
                // 否则进程会一直阻塞在密码提示上直到看门狗超时。
                st.mobile_hint_cancel.store(true, Ordering::SeqCst);
                if st.error_msg.is_none() {
                    st.error_msg = Some(
                        "登录态已失效（工具正在索要密码），请到「设置 → 账号」重新登录".into(),
                    );
                }
            }
            GuardKind::Code => {
                // 改要验证码了：手机确认推测已无意义，避免验证码弹窗和手机提示互相打架
                st.mobile_hint_cancel.store(true, Ordering::SeqCst);
                if st.guard_rx.is_none() {
                    let (tx, rx) = oneshot::channel();
                    self.guard_waiters.lock().unwrap().insert(task_id, tx);
                    st.guard_rx = Some(rx);
                    self.update(task_id, "authenticating", 0.0, None, None, true);
                    self.emit("download:guard-required", json!({ "taskId": task_id }));
                    tracing::info!("Steam Guard 验证码请求（task {task_id}）");
                }
            }
        }
    }

    /// 通知前端「请在手机 App 确认登录」，全程只发一次。
    fn notify_mobile_confirm(&self, task_id: i64, st: &TaskState, reason: &str) {
        if st
            .mobile_confirm_notified
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            tracing::info!("task {task_id}: 等待手机端确认登录（{reason}）");
            self.emit("download:mobile-confirm", json!({ "taskId": task_id }));
        }
    }

    /// 密码登录已开始：启动静默推测定时器。
    ///
    /// 新版 steamcmd 等待手机确认时不打印任何提示，用户在手机上收到推送，
    /// 应用里却毫无反馈。这里在密码登录 MOBILE_HINT_DELAY 秒后仍无进展时，
    /// 主动提示用户去手机上确认。每个任务只武装一次；登录完成/失败/改要
    /// 验证码/任务结束都会通过 mobile_hint_cancel 解除。
    fn arm_mobile_hint(&self, task_id: i64, st: &mut TaskState) {
        if st.mobile_hint_armed || st.mobile_confirm_notified.load(Ordering::SeqCst) {
            return;
        }
        st.mobile_hint_armed = true;
        let notified = st.mobile_confirm_notified.clone();
        let cancel = st.mobile_hint_cancel.clone();
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(MOBILE_HINT_DELAY).await;
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            if notified
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                tracing::info!(
                    "task {task_id}: 密码登录 {MOBILE_HINT_DELAY:?} 未完成，推测在等待手机确认"
                );
                let _ = app.emit("download:mobile-confirm", json!({ "taskId": task_id }));
            }
        });
    }

    /// 解析下载凭据。返回 (username, password, has_token)：
    /// - 有持久化登录态：password 为空字符串，steamcmd 直接用缓存凭据登录
    /// - 无令牌：返回完整账号密码（首次登录用）
    fn resolve_credentials(&self) -> Result<(String, String, bool), String> {
        let conn = self.db.lock().map_err(|e| e.to_string())?;
        let username = db::get_setting(&conn, "download_username")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                "请先到「设置 → 账号」登录 Steam 账号（需拥有 Wallpaper Engine）".to_string()
            })?;
        let has_token = db::get_setting(&conn, "download_has_token")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        if has_token {
            return Ok((username, String::new(), true));
        }
        let password = self.read_password(&username)?;
        Ok((username, password, false))
    }

    fn read_password(&self, username: &str) -> Result<String, String> {
        let dir = self.data_dir()?;
        // 首选：本地加密存储（避免每次 Keychain 授权）
        if let Some(pw) = secure_store::load(username, &dir).ok().flatten() {
            return Ok(pw);
        }
        // 回退：旧的 Keychain 凭据（历史账号），并迁移到本地存储，后续不再弹授权
        let pw =
            get_password(username).map_err(|_| "读取密码失败，请重新配置下载账号".to_string())?;
        let _ = secure_store::save(username, &pw, &dir);
        Ok(pw)
    }

    fn fail(&self, id: i64, code: &str, msg: &str) {
        tracing::error!("download task {id} failed: {code} {msg}");
        self.guard_waiters.lock().unwrap().remove(&id);
        self.update(id, "failed", 0.0, Some(code), Some(msg), false);
        self.emit_progress(id, "failed", 0.0);
    }

    /// 收编：把下载产物移入壁纸库，解析 project.json 登记类型
    fn install(&self, item_id: String) -> Result<(), String> {
        // 该 item 若是某壁纸的依赖（target_dir 记录合并目标，`|` 分隔多个，依赖链会传递），
        // 下载完成后需把产物合并回主壁纸目录
        let dependency_targets: Vec<PathBuf> = {
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
            let mut v: Vec<PathBuf> = Vec::new();
            for r in rows {
                let raw = r.map_err(|e| e.to_string())?;
                for part in raw.split('|').filter(|p| !p.is_empty()) {
                    let p = PathBuf::from(part);
                    if !v.contains(&p) {
                        v.push(p);
                    }
                }
            }
            v
        };

        let workdir = self.workdir()?;
        let steamcmd_path = workdir
            .join("steamapps/workshop/content")
            .join(APP_ID)
            .join(&item_id);
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

        // 本 item 是某壁纸声明的依赖：把内容"缺则补"合并回每个主壁纸目录
        // （不覆盖主壁纸自身文件，资产包的 materials/textures 等也能一并回填）
        for target in &dependency_targets {
            // 主壁纸可能已被用户删除：目录缺 project.json 时跳过合并，仅清理标记
            if target.join("project.json").is_file() {
                let mut fc = 0usize;
                let mut sb = 0u64;
                match merge_missing(&dest, target, &mut fc, &mut sb) {
                    Ok(_) => tracing::info!(
                        "merged dependency {item_id} -> {} ({fc} files)",
                        target.display()
                    ),
                    Err(e) => tracing::warn!("merge dependency {item_id} failed: {e}"),
                }
            }
            // 从 target_dir 列表移除已完成的目标，其余目标（依赖链上级）留待后续任务完成时合并
            let conn = self.db.lock().map_err(|e| e.to_string())?;
            let t_str = target.to_string_lossy().to_string();
            let cur: Option<String> = conn
                .query_row(
                    "SELECT target_dir FROM downloads
                     WHERE item_id = ?1 AND target_dir IS NOT NULL LIMIT 1",
                    [&item_id],
                    |r| r.get(0),
                )
                .optional()
                .unwrap_or(None);
            if let Some(c) = cur {
                let rest: Vec<&str> = c
                    .split('|')
                    .filter(|p| !p.is_empty() && *p != t_str)
                    .collect();
                let next = if rest.is_empty() {
                    None
                } else {
                    Some(rest.join("|"))
                };
                let _ = conn.execute(
                    "UPDATE downloads SET target_dir = ?2 WHERE item_id = ?1 AND target_dir = ?3",
                    rusqlite::params![item_id, next, c],
                );
            }
        }

        // 解析 project.json（一次读出 type / title / 依赖列表）
        let project = read_project_json(&dest.join("project.json"));
        let mut wtype = "unknown".to_string();
        let mut title = item_id.clone();
        if let Some(v) = &project {
            if let Some(t) = v.get("type").and_then(|t| t.as_str()) {
                wtype = crate::steam::details::infer_type_from_tags(&[t.to_string()]);
            }
            if let Some(t) = v.get("title").and_then(|t| t.as_str()) {
                title = t.to_string();
            }
        }
        // project.json 无 type 或推断不出时：回退工坊元数据（workshop_items 已存正确类型）
        let meta = crate::db::find_workshop_item(&self.db, &item_id)
            .ok()
            .flatten();
        if wtype == "unknown" {
            if let Some(m) = &meta {
                if m.r#type != "unknown" {
                    wtype = m.r#type.clone();
                }
            }
        }
        // 标签快照：本地库自己存一份，别只依赖 JOIN workshop_items ——
        // 工坊缓存可能被清理，而本地筛选要一直能用
        let tags_json = meta
            .as_ref()
            .and_then(|m| serde_json::to_string(&m.tags).ok())
            .unwrap_or_else(|| "[]".into());
        {
            let conn = self.db.lock().map_err(|e| e.to_string())?;
            let _ = conn.execute(
                "INSERT INTO library_items(item_id, title, type, size_bytes, file_count, tags)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(item_id) DO UPDATE SET title = ?2, type = ?3, size_bytes = ?4,
                     file_count = ?5, tags = ?6",
                rusqlite::params![
                    item_id,
                    title,
                    wtype,
                    size_bytes as i64,
                    file_count as i64,
                    tags_json
                ],
            );
        }

        // 声明了依赖的壁纸：本地已有的依赖直接补齐目录内容，缺的自动加入下载队列
        let dep_ids = project
            .as_ref()
            .map(|v| parse_dependency_ids(v, &item_id))
            .unwrap_or_default();
        if !dep_ids.is_empty() {
            let missing = self.settle_dependencies(&dest, &dep_ids);
            if !missing.is_empty() {
                // 合并目标 = 主壁纸目录本身 + 本 item 作为依赖时的上级目录（链条传递）
                let mut targets = dependency_targets.clone();
                targets.push(dest.clone());
                let queued = self.enqueue_dependency_tasks(&missing, &targets);
                if !queued.is_empty() {
                    tracing::info!("item {item_id}: queued dependency download {queued:?}");
                }
            }
        }
        tracing::info!(
            "installed item {item_id}: type={wtype}, files={file_count}, size={size_bytes}"
        );
        Ok(())
    }

    // ---------- 依赖处理 ----------

    /// 处理壁纸声明的依赖：本地已有的依赖内容"缺则补"合并进壁纸目录，
    /// 返回仍缺失、需要下载的依赖 ID。
    pub fn settle_dependencies(&self, item_dir: &Path, dep_ids: &[String]) -> Vec<String> {
        let wallpapers = match self.wallpapers_dir() {
            Ok(d) => d,
            Err(_) => return dep_ids.to_vec(),
        };
        let mut missing = Vec::new();
        for dep_id in dep_ids {
            let dep_dir = wallpapers.join(dep_id);
            let downloaded = dep_dir.is_dir()
                && std::fs::read_dir(&dep_dir)
                    .map(|mut d| d.next().is_some())
                    .unwrap_or(false);
            if !downloaded {
                missing.push(dep_id.clone());
                continue;
            }
            let mut fc = 0usize;
            let mut sb = 0u64;
            match merge_missing(&dep_dir, item_dir, &mut fc, &mut sb) {
                Ok(_) if fc > 0 => tracing::info!(
                    "merged existing dependency {dep_id} -> {} ({fc} files)",
                    item_dir.display()
                ),
                Err(e) => tracing::warn!("merge existing dependency {dep_id} failed: {e}"),
                _ => {}
            }
        }
        missing
    }

    /// 把缺失的依赖工坊内容加入下载队列（队列去重；target_dir 记录合并目标，`|` 分隔多个），
    /// 返回实际入队的依赖 ID。该依赖已有 pending 任务时，仅把本次合并目标补记进其 target_dir。
    pub fn enqueue_dependency_tasks(&self, dep_ids: &[String], targets: &[PathBuf]) -> Vec<String> {
        let target_s = targets
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("|");
        let mut queued = Vec::new();
        let conn = match self.db.lock() {
            Ok(c) => c,
            Err(_) => return queued,
        };
        for dep_id in dep_ids {
            let pending: Option<String> = conn
                .query_row(
                    "SELECT target_dir FROM downloads
                     WHERE item_id = ?1 AND status IN ('queued','authenticating','downloading','installing')
                     LIMIT 1",
                    [dep_id],
                    |r| r.get(0),
                )
                .optional()
                .unwrap_or(None);
            if pending.is_some() {
                if target_s.is_empty() {
                    continue;
                }
                let mut dirs: Vec<String> = pending
                    .unwrap_or_default()
                    .split('|')
                    .filter(|p| !p.is_empty())
                    .map(|p| p.to_string())
                    .collect();
                if !dirs.iter().any(|d| *d == target_s) {
                    dirs.push(target_s.clone());
                }
                let _ = conn.execute(
                    "UPDATE downloads SET target_dir = ?2
                     WHERE item_id = ?1 AND status IN ('queued','authenticating','downloading','installing')",
                    rusqlite::params![dep_id, dirs.join("|")],
                );
                continue;
            }
            let res = conn.execute(
                "INSERT INTO downloads(item_id, status, progress, target_dir, created_at)
                 VALUES (?1, 'queued', 0, ?2, unixepoch())",
                rusqlite::params![
                    dep_id,
                    if target_s.is_empty() {
                        None
                    } else {
                        Some(target_s.clone())
                    }
                ],
            );
            if res.is_ok() {
                let task_id = conn.last_insert_rowid();
                queued.push(dep_id.clone());
                // 新任务不在前端列表里，发一次事件驱动其拉取全量列表
                self.emit_progress(task_id, "queued", 0.0);
            }
        }
        drop(conn);
        if !queued.is_empty() {
            self.fetch_dependency_meta(queued.clone());
        }
        queued
    }

    /// best-effort：后台拉取依赖条目的工坊元数据写入缓存，下载页即可显示标题而非裸 ID
    fn fetch_dependency_meta(&self, dep_ids: Vec<String>) {
        let app = self.app.clone();
        let db = self.db.clone();
        tauri::async_runtime::spawn(async move {
            let Some(client) = app.try_state::<crate::steam::SteamClient>() else {
                return;
            };
            let Ok(items) = crate::steam::details::get_item_details(&client, &dep_ids).await else {
                return;
            };
            if items.is_empty() {
                return;
            }
            if let Ok(conn) = db.lock() {
                for item in &items {
                    let _ = db::upsert_workshop_item(&conn, item);
                }
            }
        });
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

/// run_task 循环内的可变状态，抽出来避免 select! 分支里传一长串 &mut 参数。
struct TaskState {
    success: bool,
    error_msg: Option<String>,
    /// 最近的非进度输出，失败时拼进错误信息辅助排查
    recent: Vec<String>,
    guard_rx: Option<oneshot::Receiver<String>>,
    exit_code: Option<i32>,
    /// 「等待手机端确认」提示已通知前端。
    /// 两个触发源（steamcmd 打印确认文案 / 静默超时推测）共享该标志做去重，
    /// 所以必须是 Arc<AtomicBool> 让推测定时器也能原子地抢这一次机会。
    mobile_confirm_notified: Arc<AtomicBool>,
    /// 静默推测定时器是否已启动（每次任务只启动一次）
    mobile_hint_armed: bool,
    /// 置位后推测定时器放弃发提示（登录已成功/失败/改要验证码/任务结束）
    mobile_hint_cancel: Arc<AtomicBool>,
}

fn copy_recursive(
    from: &Path,
    to: &Path,
    file_count: &mut usize,
    size_bytes: &mut u64,
) -> Result<(), String> {
    if from.is_dir() {
        std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
        for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            copy_recursive(
                &entry.path(),
                &to.join(entry.file_name()),
                file_count,
                size_bytes,
            )?;
        }
    } else {
        std::fs::copy(from, to).map_err(|e| e.to_string())?;
        *file_count += 1;
        *size_bytes += from.metadata().map(|m| m.len()).unwrap_or(0);
    }
    Ok(())
}

/// 读取 project.json 并解析为 JSON（真实壁纸常带 UTF-8 BOM，serde_json 不认，需先剥离）
pub(crate) fn read_project_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// 解析 project.json 里声明的依赖工坊 ID 集合。
/// WE 编辑器实际写出两种形态：顶层 `dependencies`（复数，字符串数组，资产包依赖，主流）
/// 和 `dependency`（单数，字符串，仅预设物品指向其基础壁纸），取并集；
/// 去重，过滤空值、自身与非纯数字 ID（publishedfileid 均为数字）。
pub(crate) fn parse_dependency_ids(project: &serde_json::Value, self_id: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        let s = s.trim();
        if s.is_empty() || s == self_id || !s.bytes().all(|b| b.is_ascii_digit()) {
            return;
        }
        if !ids.iter().any(|v| v == s) {
            ids.push(s.to_string());
        }
    };
    if let Some(arr) = project.get("dependencies").and_then(|d| d.as_array()) {
        for v in arr.iter().filter_map(|v| v.as_str()) {
            push(v);
        }
    }
    if let Some(s) = project.get("dependency").and_then(|d| d.as_str()) {
        push(s);
    }
    ids
}

/// 把 src 目录内容"缺则补"合并进 dst：目标已存在的文件/目录一律跳过（不覆盖主壁纸自身的
/// project.json、预览图等），仅补齐缺失部分。用于依赖产物回填主壁纸目录。
pub(crate) fn merge_missing(
    src: &Path,
    dst: &Path,
    file_count: &mut usize,
    size_bytes: &mut u64,
) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let to = dst.join(entry.file_name());
        if to.exists() {
            continue;
        }
        let from = entry.path();
        if from.is_dir() {
            merge_missing(&from, &to, file_count, size_bytes)?;
        } else {
            copy_recursive(&from, &to, file_count, size_bytes)?;
        }
    }
    Ok(())
}

// ---------- 初始化与 Tauri 命令 ----------

pub fn init(app: &AppHandle) -> Result<(), String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let svc = Arc::new(DownloadService::new(db.inner().clone(), app.clone()));
    app.manage(svc.clone());

    // 老版本用 DepotDownloader 时的登录令牌目录；该后端已移除，清掉释放空间
    if let Ok(dir) = app.path().app_data_dir() {
        let legacy = dir.join("dd-config");
        if legacy.exists() {
            let _ = std::fs::remove_dir_all(&legacy);
            tracing::info!("已清理旧 DepotDownloader 配置目录: {}", legacy.display());
        }
    }

    // 重启恢复：中断任务标记失败。
    // 例外：installing 阶段若产物已入库（安装其实已完成，只是没来得及标 done，
    // 例如进程被强杀），直接判为成功，避免让用户重下一遍大文件。
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let _ = conn.execute(
            "UPDATE downloads SET status='done', progress=100, waiting_guard=0,
                    error_code=NULL, error_msg=NULL, finished_at = unixepoch()
             WHERE status='installing'
               AND item_id IN (SELECT item_id FROM library_items)",
            [],
        );
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

/// 是否有下载相关活动进行中（队列任务 / Steam Guard 等待 / 扫码登录）。
/// 主窗口闲置释放前查询：活动期间跳过释放，避免打断 Guard 输入与进度展示。
pub fn is_busy(app: &AppHandle) -> bool {
    let Some(svc) = app.try_state::<Arc<DownloadService>>() else {
        return false;
    };
    // 内存侧：当前队列任务 / Guard 等待通道
    if svc
        .current_task
        .lock()
        .map(|t| t.is_some())
        .unwrap_or(false)
    {
        return true;
    }
    if svc
        .guard_waiters
        .lock()
        .map(|g| !g.is_empty())
        .unwrap_or(true)
    {
        return true;
    }
    // DB 侧（权威）：尚未结束的任务，或等待 Guard 输入（状态集与重启恢复一致）
    let Ok(conn) = svc.db.lock() else {
        return false;
    };
    conn.query_row(
        "SELECT COUNT(*) FROM downloads
          WHERE status IN ('queued','authenticating','downloading','installing')
             OR waiting_guard = 1",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// 用户配置的下载代理（手动代理优先，其次 Steam 专用代理），空闲时返回 None。
///
/// 各条「运行时下载」链路共用：steamcmd 引导包、托管 ffmpeg 静态包。
/// 挂在 download 模块下是因为代理本来就是这个页面的设置。
pub(crate) fn read_proxy(app: &AppHandle) -> Option<String> {
    let db = app.try_state::<Arc<Mutex<Connection>>>()?;
    let conn = db.lock().ok()?;
    db::get_setting(&conn, "download_proxy")
        .or_else(|| db::get_setting(&conn, "steam_proxy"))
        .filter(|s| !s.trim().is_empty())
}

/// 下载工具（steamcmd）安装状态，供设置页展示。
#[tauri::command]
pub fn download_tool_status(app: AppHandle) -> serde_json::Value {
    steamcmd_install::status(&app)
}

/// 安装（或修复）steamcmd。异步执行，进度经 `steamcmd:install-progress` 事件推送。
#[tauri::command]
pub async fn steamcmd_install_tool(
    app: AppHandle,
    force: Option<bool>,
) -> Result<serde_json::Value, String> {
    steamcmd_install::install(app, force.unwrap_or(false)).await
}

/// 卸载 steamcmd（删除安装目录，保留登录态目录）
#[tauri::command]
pub fn steamcmd_uninstall_tool(app: AppHandle) -> Result<(), String> {
    steamcmd_install::uninstall(&app)
}

#[tauri::command]
pub fn download_credentials_set(
    app: AppHandle,
    username: String,
    password: String,
) -> Result<serde_json::Value, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    // 首选本地加密存储（不再写 Keychain；旧 Keychain 逻辑保留在 keychain.rs）
    secure_store::save(&username, &password, &dir).map_err(|e| format!("凭据写入失败: {e}"))?;
    // 清理旧的 Keychain 凭据，避免残留（删掉失败忽略）
    let _ = crate::keychain::delete_password(&username);
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        db::set_setting(&conn, "download_username", &username)?;
        // 换账号/改密码：清除旧的登录态标记，下次下载重新用账号密码登录
        let _ = conn.execute(
            "DELETE FROM settings WHERE key IN ('download_has_token','steamcmd_logged_in')",
            [],
        );
    }
    // 订阅同步的网页会话同样作废（绑的是旧账号/旧密码）
    secure_store::clear_session(&dir);
    Ok(json!({ "ok": true, "username": username }))
}

#[tauri::command]
pub fn download_credentials_status(app: AppHandle) -> Result<serde_json::Value, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let username = db::get_setting(&conn, "download_username");
    let has_token = db::get_setting(&conn, "download_has_token")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    let dir = app.path().app_data_dir().ok();
    let has_pw = match (&username, &dir) {
        (Some(u), Some(d)) => secure_store::has(u, d),
        _ => false,
    };
    // 密码或登录态任一可用即视为已配置（首次登录成功后 steamcmd 会缓存凭据，
    // 此时本地密码可能已被清掉，但仍能免密下载）
    Ok(json!({
        "configured": username.is_some() && (has_pw || has_token),
        "username": username,
    }))
}

/// 入队一个「登录验证」任务：只跑 steamcmd +login +quit，
/// 用于「保存凭据时立即验证」——验证码/手机确认走与下载相同的全局弹框。
/// 已有进行中的验证/下载任务在跑时直接复用，不重复入队
#[tauri::command]
pub fn download_verify_login(app: AppHandle) -> Result<i64, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let exists: Option<i64> = conn
        .query_row(
            "SELECT id FROM downloads WHERE item_id = ?1 AND status IN ('queued','authenticating','downloading','installing') LIMIT 1",
            [backend::VERIFY_ITEM_ID],
            |r| r.get::<_, i64>(0),
        )
        .ok();
    if let Some(id) = exists {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO downloads(item_id, status, progress, created_at) VALUES (?1, 'queued', 0, unixepoch())",
        [backend::VERIFY_ITEM_ID],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

#[tauri::command]
pub fn download_enqueue(app: AppHandle, item_id: String) -> Result<i64, String> {
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
                    d.error_code, d.error_msg, d.waiting_guard,
                    (d.target_dir IS NOT NULL AND d.target_dir != '') AS dependency,
                    d.created_at, d.started_at, d.finished_at
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
                dependency: r.get(8)?,
                created_at: r.get(9)?,
                started_at: r.get(10)?,
                finished_at: r.get(11)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
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

/// 清除下载账号凭据（本地存储 + DB 账号 + 两个后端各自的登录态），实现「登出」
#[tauri::command]
pub fn download_credentials_clear(app: AppHandle) -> Result<(), String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    // 清本地加密存储的账号密码（逐个已知账号尝试）
    let _ = secure_store::clear_all(&dir);
    // 连同订阅同步的网页会话一起清：账号都退了，会话不该残留
    secure_store::clear_session(&dir);
    // 清 steamcmd 登录态：它把 config.vdf 写在被重定向的 HOME 下
    if let Ok(home) = steamcmd_install::home_dir(&app) {
        let _ = std::fs::remove_dir_all(home);
    }
    let conn = db.lock().map_err(|e| e.to_string())?;
    let _ = conn.execute(
        "DELETE FROM settings WHERE key IN ('download_username','download_has_token','steamcmd_logged_in')",
        [],
    );
    Ok(())
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
        .execute(
            "DELETE FROM downloads WHERE status IN ('done','failed')",
            [],
        )
        .map_err(|e| e.to_string())?;
    Ok(n as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wpem-dep-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dependency_ids_union_and_filter() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{
                "title": "t",
                "dependencies": ["111", "222", "", "abc", "111"],
                "dependency": "333",
                "file": "scene.pkg"
            }"#,
        )
        .unwrap();
        let ids = parse_dependency_ids(&v, "999");
        assert_eq!(ids, vec!["111", "222", "333"]);
    }

    #[test]
    fn dependency_ids_singular_only_and_self_filtered() {
        let v: serde_json::Value = serde_json::from_str(r#"{ "dependency": "999" }"#).unwrap();
        assert!(parse_dependency_ids(&v, "999").is_empty());

        let v2: serde_json::Value = serde_json::from_str(r#"{ "title": "t" }"#).unwrap();
        assert!(parse_dependency_ids(&v2, "999").is_empty());
    }

    #[test]
    fn read_project_json_strips_bom() {
        let d = fixture("bom");
        let p = d.join("project.json");
        std::fs::write(&p, format!("\u{feff}{{\"dependency\":\"123\"}}")).unwrap();
        let v = read_project_json(&p).expect("BOM 应被剥离后可解析");
        assert_eq!(v.get("dependency").and_then(|x| x.as_str()), Some("123"));
    }

    #[test]
    fn merge_missing_fills_only_absent_files() {
        let src = fixture("merge-src");
        let dst = fixture("merge-dst");
        // 源：已有同名文件（不应覆盖）+ 新文件 + 新目录
        std::fs::write(src.join("project.json"), "dep").unwrap();
        std::fs::write(src.join("scene.pkg"), "pkg").unwrap();
        std::fs::create_dir_all(src.join("materials")).unwrap();
        std::fs::write(src.join("materials").join("a.png"), "a").unwrap();
        // 目标：同名文件内容不同，验证不被覆盖
        std::fs::write(dst.join("project.json"), "main").unwrap();

        let mut fc = 0usize;
        let mut sb = 0u64;
        merge_missing(&src, &dst, &mut fc, &mut sb).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.join("project.json")).unwrap(),
            "main"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("scene.pkg")).unwrap(),
            "pkg"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("materials").join("a.png")).unwrap(),
            "a"
        );
        assert_eq!(fc, 2);
    }

    /// 回归：安装完成后写 settings 的那段临界区，曾在持有 db 锁时又调了一个
    /// 内部还要 lock 同一把非重入 Mutex 的辅助函数，导致自锁死 —— DB 连接被永久
    /// 占住，任务卡在「安装中」、整个应用无响应。
    ///
    /// 现在那段代码只在锁内做纯写入。本测试守住这个约束：任何在锁内重新取锁的
    /// 改动都会让它超时失败，而不是把应用挂死。
    #[test]
    fn settings_write_after_install_does_not_self_deadlock() {
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        db.lock()
            .unwrap()
            .execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();

        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (d2, db2) = (done.clone(), db.clone());
        std::thread::spawn(move || {
            // 与 run_task 收尾处同构：一次取锁，锁内只做写入，不再调用会取锁的函数
            let conn = db2.lock().unwrap();
            db::set_setting(&conn, "download_has_token", "true").unwrap();
            db::set_setting(&conn, steamcmd_install::SETTING_LOGGED_IN, "true").unwrap();
            drop(conn);
            d2.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        // 自锁死时该线程永不返回，用超时把「挂死」变成明确失败
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !done.load(std::sync::atomic::Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "写 settings 时发生自锁死：临界区内不得再调用会取 db 锁的函数"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        let conn = db.lock().unwrap();
        assert_eq!(
            db::get_setting(&conn, "download_has_token").as_deref(),
            Some("true")
        );
        assert_eq!(
            db::get_setting(&conn, steamcmd_install::SETTING_LOGGED_IN).as_deref(),
            Some("true")
        );
    }

    /// 重启恢复：卡在 installing 但产物已入库的任务应判成功，不该让用户重下大文件；
    /// 未入库的中断任务仍按失败处理。
    #[test]
    fn restart_marks_installed_task_done_and_others_failed() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE downloads(id INTEGER PRIMARY KEY, item_id TEXT, status TEXT,
                 progress REAL DEFAULT 0, error_code TEXT, error_msg TEXT,
                 waiting_guard INTEGER DEFAULT 0, finished_at INTEGER);
             CREATE TABLE library_items(item_id TEXT PRIMARY KEY);
             -- 已入库：安装其实完成了，只是没来得及标 done
             INSERT INTO downloads(id,item_id,status) VALUES (1,'aaa','installing');
             INSERT INTO library_items(item_id) VALUES ('aaa');
             -- 未入库：真正中断
             INSERT INTO downloads(id,item_id,status) VALUES (2,'bbb','installing');
             INSERT INTO downloads(id,item_id,status) VALUES (3,'ccc','downloading');",
        )
        .unwrap();

        conn.execute(
            "UPDATE downloads SET status='done', progress=100, waiting_guard=0,
                    error_code=NULL, error_msg=NULL, finished_at = unixepoch()
             WHERE status='installing'
               AND item_id IN (SELECT item_id FROM library_items)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE downloads SET status='failed', error_code='RESTARTED', error_msg='应用重启，任务中断',
             waiting_guard=0, finished_at = unixepoch()
             WHERE status IN ('queued','authenticating','downloading','installing')",
            [],
        )
        .unwrap();

        let status = |id: i64| -> String {
            conn.query_row("SELECT status FROM downloads WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
        };
        assert_eq!(status(1), "done", "产物已入库应判成功，避免重下");
        assert_eq!(status(2), "failed");
        assert_eq!(status(3), "failed");
    }
}
