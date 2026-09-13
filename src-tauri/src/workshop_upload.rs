//! 创意工坊上传（ISteamUGC，经 steamworks SDK）。
//!
//! 流程：内容暂存（staging：过滤工程杂物 + scene 工程贴图转 .tex）→ CreateItem
//! （首传）/ SubmitItemUpdate（已有 publishedfileid = 更新）→ 轮询上传进度 →
//! 成功后回写 publishedfileid（库条目进 DB，MCP 工程写 project.json 的
//! workshop.fileId，版本由创作者自行维护，库不代管历史）。
//!
//! steamworks 0.13 的 Client 是 Send+Sync，回调统一由一条专职线程
//! `run_callbacks()` 泵送；上传任务在同一线程上串行执行（Steam 不允许并发提交）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::json;
use steamworks::{
    AppId, Client, FileType, PublishedFileId, PublishedFileVisibility, UpdateStatus,
    UpdateWatchHandle,
};
use tauri::{AppHandle, Emitter, Manager};

/// Wallpaper Engine 的 Steam AppId（工坊目标）
pub const WE_APP_ID: u32 = 431960;

/// 暂存目录体积上限（与 scene.pkg 打包同口径）
const MAX_STAGING_BYTES: u64 = 2 << 30; // 2 GiB

// ---------------------------------------------------------------- 任务状态

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadJob {
    pub job_id: String,
    /// MCP 工程名（UI 上传路径为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// 本地库条目 id（MCP 工程路径为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    pub title: String,
    /// staging | creating | uploading | committing | done | failed
    pub status: String,
    /// 0-100
    pub progress: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publishedfileid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 成功后的工坊页面 URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workshop_url: Option<String>,
    pub started_at: i64,
}

/// 一次上传的元数据（调用方解析好，工作线程只管提交）
pub struct UploadMeta {
    title: String,
    description: String,
    tags: Vec<String>,
    visibility: PublishedFileVisibility,
    changelog: Option<String>,
    /// 已有工坊条目 → 更新；None → 新建
    existing_file_id: Option<PublishedFileId>,
    /// 成功后回写 fileId 的去处
    persist: PersistTarget,
}

enum PersistTarget {
    /// 本地库条目：UPDATE library_items SET publishedfileid
    LibraryItem(String),
    /// MCP 工程：project.json 的 workshop.fileId
    Project(PathBuf),
}

#[derive(Default)]
pub struct UploadState {
    jobs: Mutex<HashMap<String, UploadJob>>,
    /// Steam 客户端（懒初始化；Some 即回调泵线程已在跑）
    client: Mutex<Option<Client>>,
    /// 上传命令队列（Steam 串行提交：工作线程一次只处理一个）
    queue: Mutex<Option<mpsc::Sender<UploadCmd>>>,
}

struct UploadCmd {
    job_id: String,
    staging: PathBuf,
    meta: UploadMeta,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 更新任务状态并向前端广播（MCP 走轮询，不依赖事件）
fn patch_job<F: FnOnce(&mut UploadJob)>(app: &AppHandle, job_id: &str, f: F) {
    let state = app.state::<UploadState>();
    let snapshot = {
        let mut jobs = state.jobs.lock().expect("jobs 锁");
        let Some(job) = jobs.get_mut(job_id) else {
            return;
        };
        f(job);
        job.clone()
    };
    let _ = app.emit("workshop-upload-progress", &snapshot);
}

// ---------------------------------------------------------------- 入口

/// 解析并启动一次上传：staging 在调用方线程（阻塞池）完成，提交交给工作线程。
/// 返回 job_id。
pub fn start_upload(
    app: &AppHandle,
    project: Option<String>,
    item_id: Option<String>,
    content_dir: PathBuf,
    // 库条目的类型（video/web 单文件壁纸需在暂存目录合成 project.json；
    // 工程目录自带 project.json，传 None）
    source_type: Option<String>,
    meta: UploadMeta,
) -> Result<String, String> {
    ensure_steam_worker(app)?;

    let job_id = format!("up-{}-{:08x}", now_ms(), rand::random::<u32>());
    {
        let state = app.state::<UploadState>();
        state.jobs.lock().expect("jobs 锁").insert(
            job_id.clone(),
            UploadJob {
                job_id: job_id.clone(),
                project,
                item_id,
                title: meta.title.clone(),
                status: "staging".into(),
                progress: 0.0,
                publishedfileid: meta.existing_file_id.map(|id| id.0.to_string()),
                error: None,
                workshop_url: None,
                started_at: now_ms(),
            },
        );
    }

    // 暂存内容（fs 重活，调用方已在阻塞池）
    let staging = stage_content(&content_dir).map_err(|e| {
        patch_job(app, &job_id, |j| {
            j.status = "failed".into();
            j.error = Some(e.clone());
        });
        e
    })?;
    // 单文件壁纸（视频/网页）没有 project.json：WE 工坊内容必须带一个，
    // 按类型合成最小工程文件（只写暂存目录，不碰源文件）
    if let Err(e) = synth_project_json(&staging, source_type.as_deref(), &meta.title) {
        let _ = std::fs::remove_dir_all(&staging);
        patch_job(app, &job_id, |j| {
            j.status = "failed".into();
            j.error = Some(e.clone());
        });
        return Err(e);
    }

    let state = app.state::<UploadState>();
    let tx = state.queue.lock().expect("queue 锁").clone();
    let Some(tx) = tx else {
        return Err("Steam 上传线程未就绪".into());
    };
    tx.send(UploadCmd {
        job_id: job_id.clone(),
        staging,
        meta,
    })
    .map_err(|e| format!("上传任务投递失败: {e}"))?;
    Ok(job_id)
}

/// 查询任务（job_id 为空 = 全部任务）
pub fn job_status(app: &AppHandle, job_id: Option<&str>) -> Result<serde_json::Value, String> {
    let state = app.state::<UploadState>();
    let jobs = state.jobs.lock().expect("jobs 锁");
    match job_id.filter(|s| !s.trim().is_empty()) {
        Some(id) => {
            let job = jobs
                .get(id.trim())
                .ok_or_else(|| format!("没有这个上传任务: {id}"))?;
            Ok(serde_json::to_value(job).map_err(|e| e.to_string())?)
        }
        None => {
            let mut all: Vec<&UploadJob> = jobs.values().collect();
            all.sort_by_key(|j| j.started_at);
            Ok(serde_json::to_value(&all).map_err(|e| e.to_string())?)
        }
    }
}

// ---------------------------------------------------------------- Steam 工作线程

/// 本机 Steam 客户端是否在运行（用于区分"没开 Steam"和"开了但连不上"）
fn steam_client_running() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("pgrep")
            .arg("-x")
            .arg("Steam")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq steam.exe", "/NH"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("steam.exe"))
            .unwrap_or(false)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("pgrep")
            .arg("-x")
            .arg("steam")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", all(unix, not(target_os = "macos")))))]
    {
        false
    }
}

/// 懒初始化 Steam 连接 + 回调泵线程。失败 = Steam 未运行/未拥有 WE。
fn ensure_steam_worker(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<UploadState>();
    let mut client_slot = state.client.lock().expect("client 锁");
    if client_slot.is_some() {
        return Ok(());
    }
    let client = Client::init_app(AppId::from(WE_APP_ID)).map_err(|e| {
        // 给出可行动的提示：Steam 没开 vs 开了但连不上（未登录/未拥有 WE）
        let hint = if !steam_client_running() {
            "请先启动 Steam 客户端并登录你的账号".to_string()
        } else {
            "Steam 已在运行但连接失败：确认账号已登录，且该账号拥有 Wallpaper Engine（Steam 要求工坊上传者拥有目标游戏）".to_string()
        };
        format!("无法连接 Steam：{hint}（{e}）")
    })?;
    let (tx, rx) = mpsc::channel::<UploadCmd>();
    *state.queue.lock().expect("queue 锁") = Some(tx);

    let app2 = app.clone();
    let client_for_thread = client.clone();
    std::thread::Builder::new()
        .name("wpem-steam-ugc".into())
        .spawn(move || worker_loop(app2, client_for_thread, rx))
        .map_err(|e| format!("启动上传线程失败: {e}"))?;

    *client_slot = Some(client);
    Ok(())
}

/// 回调泵 + 上传状态机。Steam 同一时间只允许一个 item update，串行消费队列。
fn worker_loop(app: AppHandle, client: Client, rx: mpsc::Receiver<UploadCmd>) {
    let mut current: Option<ActiveUpload> = None;
    loop {
        client.run_callbacks();

        if current.is_none() {
            match rx.try_recv() {
                Ok(cmd) => {
                    current = Some(ActiveUpload::begin(&app, &client, cmd));
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }

        if let Some(a) = current.as_mut() {
            a.tick(&client);
            if a.finished {
                current = None;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
}

/// 上传两阶段：CreateItem（首传，拿 fileId）→ SubmitItemUpdate（提交内容+元数据）。
/// 已有 fileId 的更新直接进 Submitting。
enum Stage {
    Creating,
    Submitting,
}

struct ActiveUpload {
    job_id: String,
    staging: PathBuf,
    app: AppHandle,
    stage: Stage,
    watch: Option<UpdateWatchHandle>,
    /// 提交结果（回调线程经 channel 送回，run_callbacks 时触发）
    result_rx: mpsc::Receiver<Result<(PublishedFileId, bool), steamworks::SteamError>>,
    meta: UploadMeta,
    finished: bool,
}

impl ActiveUpload {
    fn begin(app: &AppHandle, client: &Client, cmd: UploadCmd) -> ActiveUpload {
        let UploadCmd {
            job_id,
            staging,
            meta,
        } = cmd;
        let (result_tx, result_rx) = mpsc::channel();
        let ugc = client.ugc();
        match meta.existing_file_id {
            Some(file_id) => {
                patch_job(app, &job_id, |j| j.status = "uploading".into());
                let watch = submit_update(ugc, file_id, &staging, &meta, result_tx);
                ActiveUpload {
                    job_id,
                    staging,
                    app: app.clone(),
                    stage: Stage::Submitting,
                    watch: Some(watch),
                    result_rx,
                    meta,
                    finished: false,
                }
            }
            None => {
                patch_job(app, &job_id, |j| j.status = "creating".into());
                ugc.create_item(AppId::from(WE_APP_ID), FileType::Community, move |res| {
                    let _ = result_tx.send(res);
                });
                ActiveUpload {
                    job_id,
                    staging,
                    app: app.clone(),
                    stage: Stage::Creating,
                    watch: None,
                    result_rx,
                    meta,
                    finished: false,
                }
            }
        }
    }

    fn tick(&mut self, client: &Client) {
        // 进度上报（仅提交阶段有 watch）
        if let Some(watch) = &self.watch {
            let (status, done, total) = watch.progress();
            let (stage, frac) = match status {
                UpdateStatus::PreparingConfig | UpdateStatus::PreparingContent => ("uploading", 0.02),
                UpdateStatus::UploadingContent => {
                    let f = if total > 0 {
                        0.02 + 0.9 * (done as f64 / total as f64)
                    } else {
                        0.4
                    };
                    ("uploading", f)
                }
                UpdateStatus::UploadingPreviewFile => ("uploading", 0.94),
                UpdateStatus::CommittingChanges => ("committing", 0.97),
                UpdateStatus::Invalid => ("uploading", 0.1),
            };
            patch_job(&self.app, &self.job_id, |j| {
                if j.status != "done" && j.status != "failed" {
                    j.status = stage.into();
                    j.progress = j.progress.max(frac * 100.0);
                }
            });
        }

        match self.result_rx.try_recv() {
            Ok(Ok((file_id, _needs_legal))) => match self.stage {
                Stage::Creating => {
                    // 拿到 fileId → 进入提交阶段
                    patch_job(&self.app, &self.job_id, |j| {
                        j.status = "uploading".into();
                        j.publishedfileid = Some(file_id.0.to_string());
                    });
                    let (result_tx, result_rx) = mpsc::channel();
                    let watch =
                        submit_update(client.ugc(), file_id, &self.staging, &self.meta, result_tx);
                    self.watch = Some(watch);
                    self.result_rx = result_rx;
                    self.stage = Stage::Submitting;
                }
                Stage::Submitting => {
                    let url = format!(
                        "https://steamcommunity.com/sharedfiles/filedetails/?id={}",
                        file_id.0
                    );
                    persist_file_id(&self.app, &self.meta.persist, file_id);
                    patch_job(&self.app, &self.job_id, |j| {
                        j.status = "done".into();
                        j.progress = 100.0;
                        j.publishedfileid = Some(file_id.0.to_string());
                        j.workshop_url = Some(url.clone());
                    });
                    tracing::info!("工坊上传完成: {} → {url}", self.job_id);
                    self.finished = true;
                }
            },
            Ok(Err(e)) => {
                patch_job(&self.app, &self.job_id, |j| {
                    j.status = "failed".into();
                    j.error = Some(format!("Steam 提交失败: {e:?}"));
                });
                tracing::error!("工坊上传失败（{}）: {e:?}", self.job_id);
                self.finished = true;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.finished = true;
            }
        }
    }
}

/// 组装 UpdateHandle 并提交；返回 watch 供进度轮询
fn submit_update(
    ugc: steamworks::UGC,
    file_id: PublishedFileId,
    staging: &Path,
    meta: &UploadMeta,
    result_tx: mpsc::Sender<Result<(PublishedFileId, bool), steamworks::SteamError>>,
) -> UpdateWatchHandle {
    let mut handle = ugc
        .start_item_update(AppId::from(WE_APP_ID), file_id)
        .content_path(staging)
        .title(&meta.title)
        .description(&meta.description)
        .visibility(meta.visibility);
    if !meta.tags.is_empty() {
        handle = handle.tags(meta.tags.clone(), false);
    }
    if let Some(preview) = find_preview(staging) {
        handle = handle.preview_path(&preview);
    }
    handle.submit(meta.changelog.as_deref(), move |res| {
        let _ = result_tx.send(res);
    })
}

fn find_preview(dir: &Path) -> Option<PathBuf> {
    [
        "preview.png",
        "preview.jpg",
        "preview.jpeg",
        "preview.gif",
        "preview.webp",
    ]
    .iter()
    .map(|n| dir.join(n))
    .find(|p| p.is_file())
}

/// 成功后回写 publishedfileid（失败只记日志，任务本身已算完成）
fn persist_file_id(app: &AppHandle, target: &PersistTarget, file_id: PublishedFileId) {
    let id = file_id.0.to_string();
    let r = match target {
        PersistTarget::LibraryItem(item_id) => {
            let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
            let r = match db.lock() {
                Ok(conn) => conn
                    .execute(
                        "UPDATE library_items SET publishedfileid = ?2 WHERE item_id = ?1",
                        rusqlite::params![item_id, id],
                    )
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            r
        }
        PersistTarget::Project(dir) => write_project_file_id(dir, &id),
    };
    if let Err(e) = r {
        tracing::warn!("回写 publishedfileid 失败（不影响上传结果）: {e}");
    }
}

/// project.json 写入 workshop.fileId（不动 version 字段——版本由创作者维护）
fn write_project_file_id(dir: &Path, file_id: &str) -> Result<(), String> {
    let path = dir.join("project.json");
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    v["workshop"]["fileId"] = json!(file_id);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- 内容暂存

/// 把壁纸目录拷进临时 staging 目录（工坊提交的是一份快照，不锁源目录）：
/// - 跳过隐藏文件/目录（.version/、.git/…）、scene.pkg（生成物）、we-props/（用户覆盖）
/// - scene 工程：materials 下的 png/jpeg 就地转成 .tex（WE 工坊格式，复用打包的同款转换器）
fn stage_content(src: &Path) -> Result<PathBuf, String> {
    if !src.is_dir() {
        return Err(format!("内容目录不存在: {}", src.display()));
    }
    let staging = std::env::temp_dir().join(format!(
        "wpem-upload-{}-{:08x}",
        std::process::id(),
        rand::random::<u32>()
    ));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;

    let r = stage_walk(src, &staging, 0);
    if let Err(e) = r {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    Ok(staging)
}

fn stage_walk(dir: &Path, dest: &Path, depth: u32) -> Result<(), String> {
    if depth > 16 {
        return Err("内容目录嵌套过深，疑似异常目录".into());
    }
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        // 用户属性覆盖与库内生成物不上传
        if name == "we-props" || name == "scene.pkg" {
            continue;
        }
        let path = entry.path();
        let target = dest.join(&name);
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => stage_walk(&path, &target, depth + 1)?,
            Ok(ft) if ft.is_file() => {
                let ext = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                // scene 工程的 materials 贴图：png/jpeg → .tex（WE 工坊不认裸 PNG）
                if dir.file_name().map(|n| n == "materials").unwrap_or(false)
                    && matches!(ext.as_str(), "png" | "jpg" | "jpeg")
                {
                    let bytes =
                        std::fs::read(&path).map_err(|e| format!("读取 {name} 失败: {e}"))?;
                    let tex = crate::workspace::tex_from_image(&bytes, &ext)?;
                    let stem = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| name.clone());
                    std::fs::write(
                        target.with_file_name(format!("{stem}.tex")),
                        tex,
                    )
                    .map_err(|e| format!("写出 {stem}.tex 失败: {e}"))?;
                } else {
                    std::fs::copy(&path, &target)
                        .map_err(|e| format!("复制 {name} 失败: {e}"))?;
                }
            }
            _ => {}
        }
    }
    // 体积兜底
    let total = stage_size(dest);
    if total > MAX_STAGING_BYTES {
        return Err(format!("暂存内容超过上限 {} 字节", MAX_STAGING_BYTES));
    }
    Ok(())
}

/// 单文件壁纸（视频/网页）在库内没有 project.json，而 WE 工坊内容必须带一个：
/// 在暂存目录里合成最小工程文件。合不出来（找不到入口文件）时静默跳过，
/// 让 Steam 侧给出真正的错误。
fn synth_project_json(staging: &Path, source_type: Option<&str>, title: &str) -> Result<(), String> {
    if staging.join("project.json").is_file() {
        return Ok(());
    }
    let (wtype, entry) = match source_type {
        Some("video") => {
            let Some(video) = crate::library::first_video_file(staging) else {
                return Ok(());
            };
            let name = video
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            ("video", name)
        }
        Some("web") if staging.join("index.html").is_file() => ("web", "index.html".into()),
        _ => return Ok(()),
    };
    let project = json!({
        "type": wtype,
        "file": entry,
        "title": title,
    });
    std::fs::write(
        staging.join("project.json"),
        serde_json::to_string_pretty(&project).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn stage_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            match e.file_type() {
                Ok(ft) if ft.is_dir() => total += stage_size(&e.path()),
                Ok(ft) if ft.is_file() => {
                    total += e.metadata().map(|m| m.len()).unwrap_or(0);
                }
                _ => {}
            }
        }
    }
    total
}

// ---------------------------------------------------------------- 元数据解析

/// 本地库条目 → (内容目录, 元数据)。fileId 读 DB 的 publishedfileid 列。
pub fn resolve_library_item(
    app: &AppHandle,
    item_id: &str,
    title_override: Option<String>,
    description: Option<String>,
    tags: Option<Vec<String>>,
    visibility: PublishedFileVisibility,
    changelog: Option<String>,
) -> Result<(PathBuf, Option<String>, UploadMeta), String> {
    crate::library::check_item_id(item_id)?;
    let db = app.state::<Arc<Mutex<rusqlite::Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT COALESCE(l.title,''), COALESCE(l.type,''), l.publishedfileid
             FROM library_items l WHERE l.item_id = ?1",
            [item_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let Some((db_title, wtype, file_id)) = row else {
        return Err(format!("本地库里没有条目: {item_id}"));
    };
    let root = crate::library::wallpapers_dir(app)?;
    let dir = crate::library::resolved_item_dir_in(&conn, &root, item_id);
    drop(conn);
    if !dir.is_dir() {
        return Err(format!("条目内容目录不存在: {}", dir.display()));
    }

    let (_ptype, ptitle) = project_json_info(&dir);
    let title = title_override
        .filter(|s| !s.trim().is_empty())
        .or_else(|| (!ptitle.is_empty() && ptitle != "未命名").then_some(ptitle))
        .filter(|s| !s.is_empty())
        .unwrap_or(db_title);
    let existing = file_id
        .filter(|s| !s.trim().is_empty())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(PublishedFileId);

    Ok((
        dir,
        Some(wtype),
        UploadMeta {
            title,
            description: description.unwrap_or_default(),
            tags: tags.unwrap_or_default(),
            visibility,
            changelog,
            existing_file_id: existing,
            persist: PersistTarget::LibraryItem(item_id.to_string()),
        },
    ))
}

/// MCP 工程目录 → (内容目录, 元数据)。fileId 读 project.json 的 workshop.fileId。
pub fn resolve_project(
    app: &AppHandle,
    name: &str,
    title_override: Option<String>,
    description: Option<String>,
    tags_override: Option<Vec<String>>,
    visibility: PublishedFileVisibility,
    changelog: Option<String>,
) -> Result<(PathBuf, Option<String>, UploadMeta), String> {
    let dir = crate::workspace::project_dir(app, name)?;
    if !dir.is_dir() {
        return Err(format!("工程不存在: {name}"));
    }
    let (_ptype, ptitle) = project_json_info(&dir);
    let title = title_override
        .filter(|s| !s.trim().is_empty())
        .or_else(|| (!ptitle.is_empty() && ptitle != "未命名").then_some(ptitle))
        .filter(|s| !s.is_empty())
        .ok_or("工程缺少 title（project.json 或参数里给一个）")?;

    // 标签：参数优先，缺省读 project.json 的 tags（含年龄分级，逐字符与 Steam 一致）
    let tags = match tags_override {
        Some(t) if !t.is_empty() => t,
        _ => crate::library::project_tags_from(&dir),
    };
    let existing = read_project_file_id(&dir);

    Ok((
        dir.clone(),
        None,
        UploadMeta {
            title,
            description: description.unwrap_or_default(),
            tags,
            visibility,
            changelog,
            existing_file_id: existing,
            persist: PersistTarget::Project(dir),
        },
    ))
}

/// 读工程/条目目录的 project.json：返回 (type, title)
fn project_json_info(dir: &Path) -> (String, String) {
    let Ok(text) = std::fs::read_to_string(dir.join("project.json")) else {
        return ("unknown".into(), String::new());
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return ("unknown".into(), String::new());
    };
    (
        v.get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown")
            .into(),
        v.get("title").and_then(|t| t.as_str()).unwrap_or("").into(),
    )
}

fn read_project_file_id(dir: &Path) -> Option<PublishedFileId> {
    let text = std::fs::read_to_string(dir.join("project.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let raw = v.get("workshop")?.get("fileId")?.clone();
    let s = match raw {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        _ => return None,
    };
    s.trim().parse::<u64>().ok().map(PublishedFileId)
}

/// 可见性参数 → Steam 枚举（缺省公开）
pub fn parse_visibility(v: Option<&str>) -> Result<PublishedFileVisibility, String> {
    match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") | Some("public") => Ok(PublishedFileVisibility::Public),
        Some("friends") | Some("friendsonly") => Ok(PublishedFileVisibility::FriendsOnly),
        Some("private") => Ok(PublishedFileVisibility::Private),
        Some(other) => Err(format!("未知可见性: {other}（public/friends/private）")),
    }
}

// ---------------------------------------------------------------- Tauri 命令

/// 从本地库条目或 MCP 工程发起工坊上传（二选一）。返回 { jobId }。
#[tauri::command]
pub async fn workshop_upload_start(
    app: AppHandle,
    item_id: Option<String>,
    project: Option<String>,
    title: Option<String>,
    description: Option<String>,
    tags: Option<Vec<String>>,
    visibility: Option<String>,
    changelog: Option<String>,
) -> Result<serde_json::Value, String> {
    let vis = parse_visibility(visibility.as_deref())?;
    let started = match (
        item_id.filter(|s| !s.trim().is_empty()),
        project.filter(|s| !s.trim().is_empty()),
    ) {
        (Some(_), Some(_)) => Err("item_id 与 project 只能二选一".into()),
        (None, None) => Err("需要 item_id（本地库条目）或 project（工程名）之一".into()),
        (Some(item_id), None) => {
            let item = item_id.trim().to_string();
            let app2 = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let (dir, wtype, meta) =
                    resolve_library_item(&app2, &item, title, description, tags, vis, changelog)?;
                start_upload(&app2, None, Some(item), dir, wtype, meta)
            })
            .await
            .map_err(|e| e.to_string())?
        }
        (None, Some(project)) => {
            let project = project.trim().to_string();
            let app2 = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let (dir, _wtype, meta) =
                    resolve_project(&app2, &project, title, description, tags, vis, changelog)?;
                start_upload(&app2, Some(project), None, dir, _wtype, meta)
            })
            .await
            .map_err(|e| e.to_string())?
        }
    }?;
    Ok(json!({ "jobId": started }))
}

/// 查询上传任务状态（job_id 缺省 = 全部）
#[tauri::command]
pub async fn workshop_upload_status(
    app: AppHandle,
    job_id: Option<String>,
) -> Result<serde_json::Value, String> {
    job_status(&app, job_id.as_deref())
}
