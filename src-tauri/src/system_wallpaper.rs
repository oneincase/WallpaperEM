//! 系统静态壁纸同步：应用动态壁纸后，自动把「首帧/代表帧」设为系统桌面
//! 静态壁纸 —— 锁屏、登录窗口、壁纸引擎未运行时与桌面视觉一致。
//!
//! 平台实现：macOS 用 AVFoundation 抽帧 + NSWorkspace 设置；
//! Linux / Windows 用 ffmpeg 抽帧（应用内可一键安装托管副本，见 `crate::ffmpeg`），
//! 前者按 XDG_CURRENT_DESKTOP 分发到 GNOME/Cinnamon/MATE/XFCE/KDE/swww/feh 等设置途径，
//! 后者用 SystemParametersInfoW(SPI_SETDESKWALLPAPER)（见文件末尾各平台 imp）。
//!
//! 默认开启（设置页「自动设置系统壁纸」可关，键 wallpaper_auto_system_static）。
//! 抽帧方式按类型：video → AVAssetImageGenerator 首帧；gif → 首帧转 PNG；
//! image → 原图直用；scene/web → 等渲染器挂载成功后对壁纸窗口 WKWebView
//! 实拍截图（工坊预览图与实际渲染差距太大，弃用）。
//! video/gif/image 的抽帧 + PNG 编码放后台线程（解码是一帧十毫秒级，但 PNG deflate
//! 在 4K 下可到几百 ms，压在主线程会冻 UI 并被监控探测判成事件循环卡死），
//! 只有「设为桌面」那一步回主线程（NSWorkspace 有主线程要求）；
//! scene/web 必须等渲染完成，走 tokio 异步编排，不阻塞 apply。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager};

use crate::db;

/// 应用壁纸成功后调用（apply_on_main 尾部，主线程）。失败只记日志，不影响应用流程
pub fn sync_after_apply(
    app: &AppHandle,
    cfg_type: &str,
    cfg_src: Option<&str>,
    item_id: Option<&str>,
) {
    if !enabled(app) {
        return;
    }
    // item_id 优先取显式参数；直接配置应用（测试面板等）时从内容服务器 URL 解析
    let item_id = item_id
        .map(|s| s.to_string())
        .or_else(|| cfg_src.and_then(item_id_from_media_url));
    let Some(item_id) = item_id else {
        return;
    };
    // 引用模式条目：内容目录是源目录
    let Ok(dir) = crate::library::item_dir(&app, &item_id) else {
        return;
    };
    // scene/web：没有可直接解码的媒体文件，等渲染器 ready 后实拍截图
    if matches!(cfg_type, "scene" | "web") {
        spawn_snapshot_sync(app, cfg_type, &item_id, &dir);
        return;
    }
    // video/gif/image：抽帧是硬件解码（十毫秒级），但 poster_frame 里还包含 **PNG 编码**
    // （4K 一帧的 deflate 可到几百 ms）。本函数由 apply_on_main 在主线程调用，编码压在
    // 主线程上会冻住 UI 与壁纸动画，也会被壁纸监控的「事件循环是否卡死」探测抓成
    // `UI event loop wedged?`（与截图那条同源）。所以抽帧整体丢后台线程，
    // 只有「设为桌面」那一步回主线程 —— NSWorkspace 那条路有主线程要求（见 set_all_screens）。
    let app = app.clone();
    let ty = cfg_type.to_string();
    std::thread::spawn(move || {
        let img = {
            // 抽帧产物写的是同一个路径，且 write_png 是 fs::write 直写：两个 apply 撞在
            // 一起会互相截断。只锁抽帧这一段 —— 回主线程的 hop 不持锁，否则后来的 apply
            // 会白白排队等一次 UI 往返。
            let _guard = POSTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            match poster_frame(&dir, &item_id, &ty) {
                Ok(img) => img,
                Err(e) => {
                    tracing::warn!("system wallpaper: poster frame failed: {e}");
                    return;
                }
            }
        };
        if let Err(e) = app.run_on_main_thread(move || match set_all_screens(&img) {
            Ok(()) => tracing::info!("system wallpaper: synced ({}, {})", ty, img.display()),
            Err(e) => tracing::warn!("system wallpaper: set failed: {e}"),
        }) {
            tracing::warn!("system wallpaper: 派发设壁纸到主线程失败: {e}");
        }
    });
}

/// 抽帧缓存的写盘串行化锁（理由见 sync_after_apply 里那段注释）
static POSTER_LOCK: Mutex<()> = Mutex::new(());

/// 设置开关：默认开启（键未写入过 = true）
fn enabled(app: &AppHandle) -> bool {
    let Some(db) = app.try_state::<Arc<Mutex<rusqlite::Connection>>>() else {
        return true;
    };
    let Ok(conn) = db.lock() else { return true };
    db::get_setting(&conn, "wallpaper_auto_system_static")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(true)
}

/// 从内容服务器 URL（.../media/<token>/<item_id>/<file>）解析 item_id
fn item_id_from_media_url(src: &str) -> Option<String> {
    let rest = src.split("/media/").nth(1)?;
    let mut parts = rest.split('/');
    let _token = parts.next()?;
    let item = parts.next()?;
    if item.is_empty() {
        None
    } else {
        Some(item.to_string())
    }
}

/// scene/web 的异步截图编排：等渲染器挂载成功（/diag 回流的 ready 信号），
/// 再多等一拍让场景/网页充分渲染，然后对壁纸窗口 WKWebView 实拍截图。
/// apply 流程在主线程，mount 需要秒级时间，绝不能阻塞 —— 整体 tokio 编排
#[cfg(target_os = "macos")]
fn spawn_snapshot_sync(app: &AppHandle, cfg_type: &str, item_id: &str, dir: &Path) {
    use std::sync::atomic::Ordering;

    // 找到挂着该类型壁纸的窗口（取第一个匹配；窗口可能随布局变化重建，
    // 真正截图时再解析一次，这里只是确认现在有一扇）
    let Some(engine) = app.try_state::<crate::wallpaper::WallpaperEngineState>() else {
        return;
    };
    let label = {
        let windows = engine.windows.lock().unwrap();
        windows
            .iter()
            .find(|(_, c)| c.r#type == cfg_type)
            .or_else(|| windows.iter().next())
            .map(|(l, _)| l.clone())
    };
    let Some(label) = label else { return };
    let Some(server) = app.try_state::<crate::content_server::ContentServerState>() else {
        return;
    };
    let ready_ms = server.wallpaper_ready_ms.clone();
    let out = dir.join(format!("system-wallpaper-{item_id}.png"));
    let app2 = app.clone();
    let ty = cfg_type.to_string();
    tauri::async_runtime::spawn(async move {
        // 等「新一次」ready：t0 之前的时间戳属于上一张壁纸
        let t0 = ready_ms.load(Ordering::Relaxed);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            if ready_ms.load(Ordering::Relaxed) > t0 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                tracing::warn!("system wallpaper: 等渲染 ready 超时，跳过 {ty} 截图");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        // ready 时首帧刚上屏；再等一拍让场景多渲染几帧，避免截到半成品画面
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        // 无缝切换后现役窗可能是 `-b` 变体：按基 label 过解析层拿现役的那扇
        let Some(window) = crate::wallpaper::wallpaper_window(&app2, &label) else {
            tracing::warn!("system wallpaper: 壁纸窗口 {label} 已不存在，跳过截图");
            return;
        };
        // with_webview 派发到主线程执行；completion 也回主线程 → mpsc 回传 PNG
        let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, String>>();
        if let Err(e) = window.with_webview(move |wv| imp::snapshot_webview(&wv, tx)) {
            tracing::warn!("system wallpaper: 截图派发失败: {e}");
            return;
        }
        // 在阻塞线程上等 completion（WebContent 僵死时 15s 超时兜底）
        let png = tauri::async_runtime::spawn_blocking(move || {
            rx.recv_timeout(std::time::Duration::from_secs(15))
                .map_err(|_| "截图超时（WebContent 无响应）".to_string())
                .and_then(|r| r)
        })
        .await;
        let png = match png {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(e)) => {
                tracing::warn!("system wallpaper: 截图失败: {e}");
                return;
            }
            Err(e) => {
                tracing::warn!("system wallpaper: 截图等待失败: {e}");
                return;
            }
        };
        if let Err(e) = std::fs::write(&out, &png) {
            tracing::warn!("system wallpaper: 写截图缓存失败: {e}");
            return;
        }
        // NSWorkspace.setDesktopImageURL 必须在主线程
        let ty2 = ty.clone();
        let _ = app2.run_on_main_thread(move || {
            if let Err(e) = set_all_screens(&out) {
                tracing::warn!("system wallpaper: set failed: {e}");
            } else {
                tracing::info!("system wallpaper: synced ({ty2}, {})", out.display());
            }
        });
    });
}

#[cfg(not(target_os = "macos"))]
fn spawn_snapshot_sync(_: &AppHandle, _: &str, _: &str, _: &Path) {}

/// 找出该条目的「代表帧」图片（必要时抽帧/转码到缓存目录）
fn poster_frame(dir: &Path, item_id: &str, ty: &str) -> Result<PathBuf, String> {
    match ty {
        "video" => {
            let video =
                find_file(dir, &[".mp4", ".webm", ".mov", ".m4v"]).ok_or("目录内无视频文件")?;
            let out = cached_png(dir, item_id, &video)?;
            extract_video_frame(&video, &out)?;
            Ok(out)
        }
        "gif" => {
            let gif = find_file(dir, &[".gif"]).ok_or("目录内无 GIF 文件")?;
            let out = cached_png(dir, item_id, &gif)?;
            extract_image_frame(&gif, &out)?;
            Ok(out)
        }
        // 静态图：原图直用（NSWorkspace 支持 jpg/png）
        "image" => {
            find_file(dir, &[".png", ".jpg", ".jpeg"]).ok_or_else(|| "目录内无图片文件".to_string())
        }
        // scene/web 不走这里（spawn_snapshot_sync 实拍截图）
        _ => Err(format!("不支持的壁纸类型: {ty}")),
    }
}

/// 抽帧产物的缓存路径（新鲜度判断在 cache_fresh，由抽帧函数自己短路）
/// 供库导入复用：视频抽首帧写 PNG（本地库卡片封面）。
/// macOS = AVFoundation（mp4/mov/m4v 等系统可解码格式；mkv/avi/webm 可能失败）；
/// Linux / Windows = 系统 ffmpeg（未安装则 Err）；其余平台不支持。失败由调用方自行降级
/// （导入不该因没有封面而失败）。
pub(crate) fn video_poster_png(video: &Path, out: &Path) -> Result<(), String> {
    extract_video_frame(video, out)
}

fn cached_png(dir: &Path, item_id: &str, _source: &Path) -> Result<PathBuf, String> {
    Ok(dir.join(format!("system-wallpaper-{item_id}.png")))
}

/// 缓存是否仍新鲜（调用方据此跳过抽帧；仅 macOS imp 使用）
#[cfg(target_os = "macos")]
fn cache_fresh(out: &Path, source: &Path) -> bool {
    let (Ok(a), Ok(b)) = (
        std::fs::metadata(source).and_then(|m| m.modified()),
        std::fs::metadata(out).and_then(|m| m.modified()),
    ) else {
        return false;
    };
    b >= a
}

fn find_file(dir: &Path, exts_or_names: &[&str]) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if exts_or_names.iter().any(|x| lower.ends_with(x)) {
            return Some(e.path());
        }
    }
    None
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::AnyThread;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSScreen, NSWorkspace};
    use objc2_core_graphics::CGImage;
    use objc2_foundation::{NSData, NSDictionary, NSError, NSRect, NSString, NSURL};

    // CMTime 直接用 objc2-core-media（项目已是直接依赖，无需手工 repr(C)）
    use objc2_core_media::{kCMTimeZero, CMTime};

    // 直接用裸 msg_send 调 AVFoundation（不新增 objc2-av-foundation 依赖）
    #[link(name = "AVFoundation", kind = "framework")]
    unsafe extern "C" {}

    unsafe extern "C" {
        fn CFRelease(cf: *mut std::ffi::c_void);
    }

    /// video：AVAssetImageGenerator 抽首帧写 PNG
    pub fn extract_video_frame(video: &Path, out: &Path) -> Result<(), String> {
        if cache_fresh(out, video) {
            return Ok(());
        }
        unsafe {
            let url = NSURL::fileURLWithPath(&NSString::from_str(&video.to_string_lossy()));
            let asset_cls = AnyClass::get(c"AVAsset").ok_or("AVAsset 类不可用")?;
            let asset: *mut AnyObject = msg_send![asset_cls, assetWithURL: &*url];
            if asset.is_null() {
                return Err("AVAsset 打开失败".into());
            }
            let gen_cls =
                AnyClass::get(c"AVAssetImageGenerator").ok_or("AVAssetImageGenerator 类不可用")?;
            let gen: *mut AnyObject = msg_send![gen_cls, assetImageGeneratorWithAsset: asset];
            if gen.is_null() {
                return Err("抽帧器创建失败".into());
            }
            let _: () = msg_send![gen, setAppliesPreferredTrackTransform: true];
            // 容差 0 = 精确到帧（拿真正的首帧而不是近似关键帧）
            let _: () = msg_send![gen, setRequestedTimeToleranceBefore: kCMTimeZero];
            let _: () = msg_send![gen, setRequestedTimeToleranceAfter: kCMTimeZero];
            let mut err: *mut NSError = std::ptr::null_mut();
            // copyCGImageAtTime 是 Create 规则返回 +1：交给 NSBitmapImageRep 后 CFRelease
            let cg: *mut CGImage = msg_send![gen, copyCGImageAtTime: kCMTimeZero, actualTime: std::ptr::null_mut::<CMTime>(), error: &mut err];
            if cg.is_null() {
                return Err(if err.is_null() {
                    "首帧解码失败".into()
                } else {
                    format!("首帧解码失败: {}", (*err).localizedDescription())
                });
            }
            let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &*cg);
            CFRelease(cg as *mut _);
            write_png(&rep, out)
        }
    }

    /// gif（或其他位图）：NSBitmapImageRep 解码首帧转 PNG
    pub fn extract_image_frame(src: &Path, out: &Path) -> Result<(), String> {
        if cache_fresh(out, src) {
            return Ok(());
        }
        unsafe {
            let data = std::fs::read(src).map_err(|e| e.to_string())?;
            let data = NSData::with_bytes(&data);
            let Some(rep) = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &data) else {
                return Err("图片解码失败".into());
            };
            write_png(&rep, out)
        }
    }

    /// scene/web：对壁纸窗口的 WKWebView 实拍截图，PNG 字节经 channel 回传。
    /// 由 with_webview 派发到主线程调用；completion 同样回主线程。
    /// WKWebView/WKSnapshotConfiguration 走裸 msg_send（不新增 objc2-web-kit 依赖；
    /// WebKit 框架本就已随 webview 加载进进程，AnyClass 直接查得到）
    pub fn snapshot_webview(
        wv: &tauri::webview::PlatformWebview,
        tx: std::sync::mpsc::Sender<Result<Vec<u8>, String>>,
    ) {
        unsafe {
            let webview = wv.inner() as *mut AnyObject;
            if webview.is_null() {
                let _ = tx.send(Err("WKWebView 指针为空".into()));
                return;
            }
            let Some(config_cls) = AnyClass::get(c"WKSnapshotConfiguration") else {
                let _ = tx.send(Err("WKSnapshotConfiguration 类不可用".into()));
                return;
            };
            // new 是 Create 规则 +1：包成 Retained 自动释放；截整个可见区域
            let config: objc2::rc::Retained<AnyObject> = msg_send![config_cls, new];
            let bounds: NSRect = msg_send![webview, bounds];
            let _: () = msg_send![&*config, setRect: bounds];
            // completion 只在主线程回调，而 3024×1964 的 PNG deflate 实测 >100ms：
            // 压在主线程上会冻住 UI 与壁纸动画，还会被壁纸监控的「事件循环是否卡死」
            // 探测抓成 `UI event loop wedged?`（实测就是本函数引起的）。所以主线程只
            // 取位图，压缩丢给后台线程。
            // block2 要求闭包是 Fn（不能把 tx 直接 move 出去），用 Option 包一层取走。
            let tx = std::sync::Mutex::new(Some(tx));
            let block = block2::RcBlock::new(move |image: *mut AnyObject, error: *mut NSError| {
                let Some(tx) = tx.lock().ok().and_then(|mut g| g.take()) else {
                    return; // WebKit 只回调一次；重复回调时 channel 已交出，忽略
                };
                let started = std::time::Instant::now();
                let tiff = snapshot_tiff_bytes(image, error);
                tracing::debug!(
                    "snapshot: 位图抽取耗时 {}ms（主线程）",
                    started.elapsed().as_millis()
                );
                match tiff {
                    Ok(bytes) => {
                        std::thread::spawn(move || {
                            let _ = tx.send(encode_png_from_tiff(&bytes));
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
            });
            let _: () = msg_send![
                webview,
                takeSnapshotWithConfiguration: &*config,
                completionHandler: &*block
            ];
        }
    }

    /// 主线程侧：NSImage（+0，仅回调内有效）→ TIFF 字节。
    ///
    /// 只做这一层：NSImage 不是线程安全的，必须在回调里就地取；而 TIFF 只是
    /// 「原始位图 + 头」的拷贝，比 PNG deflate 便宜一个量级。
    unsafe fn snapshot_tiff_bytes(
        image: *mut AnyObject,
        error: *mut NSError,
    ) -> Result<Vec<u8>, String> {
        unsafe {
            if !error.is_null() {
                return Err(format!(
                    "WebKit 截图错误: {}",
                    (*error).localizedDescription()
                ));
            }
            if image.is_null() {
                return Err("WebKit 截图返回空图像".into());
            }
            let tiff: *mut NSData = msg_send![image, TIFFRepresentation];
            if tiff.is_null() {
                return Err("截图 TIFF 表示为空".into());
            }
            // 拷成 Vec 再跨线程：NSData 是 autorelease 的临时对象，出不了回调作用域
            Ok((*tiff).to_vec())
        }
    }

    /// 后台线程侧：TIFF 字节 → PNG 字节。
    /// NSBitmapImageRep 的解码/编码不依赖主线程（依赖主线程的是 NSImage），
    /// 放后台跑掉这段 deflate。
    fn encode_png_from_tiff(tiff: &[u8]) -> Result<Vec<u8>, String> {
        unsafe {
            let data = NSData::with_bytes(tiff);
            let Some(rep) = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &data)
            else {
                return Err("截图位图解码失败".into());
            };
            rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
                .map(|d| d.to_vec())
                .ok_or_else(|| "截图 PNG 编码失败".into())
        }
    }

    unsafe fn write_png(rep: &NSBitmapImageRep, out: &Path) -> Result<(), String> {
        let png = rep
            .representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
            .ok_or("PNG 编码失败")?;
        std::fs::write(out, png.to_vec()).map_err(|e| format!("写缓存失败: {e}"))
    }

    /// 设为所有显示器的桌面静态壁纸。
    /// 注：setDesktopImageURL 是 Apple 已标记废弃的 API（macOS 26 推 WallpaperKit
    /// 扩展路线），但目前仍是唯一无需系统扩展即可用的方式，vidwall 等同类项目
    /// 也都是这条路；失败只记日志。
    pub fn set_all_screens(img: &Path) -> Result<(), String> {
        let mtm = MainThreadMarker::new().ok_or("set_all_screens 须在主线程")?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(&img.to_string_lossy()));
        let ws = NSWorkspace::sharedWorkspace();
        let screens = NSScreen::screens(mtm);
        let mut first_err: Option<String> = None;
        for screen in screens.iter() {
            unsafe {
                if let Err(e) = ws.setDesktopImageURL_forScreen_options_error(
                    &url,
                    &screen,
                    &NSDictionary::new(),
                ) {
                    first_err.get_or_insert_with(|| e.localizedDescription().to_string());
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[cfg(target_os = "macos")]
use imp::{extract_image_frame, extract_video_frame, set_all_screens};

// ---------- Linux：ffmpeg 抽帧 + 按桌面环境设置 ----------

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::process::Command;

    /// video/gif → 首帧 PNG。命令由 `crate::ffmpeg` 决定：应用内装的托管副本优先，
    /// 其次是 PATH 上的系统 ffmpeg（发行版仓库大多有；不想装就点设置页的「安装」）。
    fn ffmpeg_first_frame(input: &Path, out: &Path) -> Result<(), String> {
        let r = crate::ffmpeg::command()
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .arg("-i")
            .arg(input)
            .args(["-frames:v", "1"])
            .arg(out)
            .output();
        match r {
            Ok(o) if o.status.success() && out.is_file() => Ok(()),
            Ok(o) => Err(format!(
                "ffmpeg 抽帧失败: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(_) => Err(format!("未找到 ffmpeg：{}", crate::ffmpeg::MISSING_HINT)),
        }
    }

    pub fn extract_video_frame(input: &Path, out: &Path) -> Result<(), String> {
        ffmpeg_first_frame(input, out)
    }

    pub fn extract_image_frame(input: &Path, out: &Path) -> Result<(), String> {
        ffmpeg_first_frame(input, out)
    }

    fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
        let out = cmd.output().map_err(|e| format!("{what}: 启动失败: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{what}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    fn which(cmd: &str) -> bool {
        Command::new("sh")
            .args(["-c", &format!("command -v {cmd} >/dev/null 2>&1")])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// 按桌面环境把 img 设为所有屏幕的静态壁纸（best-effort，逐项尝试）
    pub fn set_all_screens(img: &Path) -> Result<(), String> {
        let desktops = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let d = desktops.to_ascii_uppercase();
        let uri = url::Url::from_file_path(img)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| format!("file://{}", img.display()));
        let path = img.display().to_string();

        if d.contains("GNOME")
            || d.contains("BUDGIE")
            || d.contains("PANTHEON")
            || d.contains("UNITY")
        {
            // GNOME 系：明/暗两套键都要设，否则深色模式下仍是旧图
            run(
                Command::new("gsettings").args([
                    "set",
                    "org.gnome.desktop.background",
                    "picture-uri",
                    &uri,
                ]),
                "gsettings picture-uri",
            )?;
            let _ = run(
                Command::new("gsettings").args([
                    "set",
                    "org.gnome.desktop.background",
                    "picture-uri-dark",
                    &uri,
                ]),
                "gsettings picture-uri-dark",
            );
            return Ok(());
        }
        if d.contains("CINNAMON") {
            return run(
                Command::new("gsettings").args([
                    "set",
                    "org.cinnamon.desktop.background",
                    "picture-uri",
                    &uri,
                ]),
                "gsettings cinnamon",
            );
        }
        if d.contains("MATE") {
            return run(
                Command::new("gsettings").args([
                    "set",
                    "org.mate.background",
                    "picture-filename",
                    &path,
                ]),
                "gsettings mate",
            );
        }
        if d.contains("XFCE") {
            // XFCE 每显示器每工作区各一条 last-image 属性：枚举后逐条写
            let out = Command::new("xfconf-query")
                .args(["-c", "xfce4-desktop", "-l"])
                .output()
                .map_err(|e| format!("xfconf-query: 启动失败: {e}"))?;
            let props: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| l.ends_with("last-image"))
                .map(|l| l.trim().to_string())
                .collect();
            if props.is_empty() {
                return Err("xfconf-query 未列出任何壁纸属性".into());
            }
            for p in props {
                let _ = run(
                    Command::new("xfconf-query").args([
                        "-c",
                        "xfce4-desktop",
                        "-p",
                        &p,
                        "-s",
                        &path,
                    ]),
                    "xfconf-query set",
                );
            }
            return Ok(());
        }
        if d.contains("KDE") {
            if which("plasma-apply-wallpaperimage") {
                return run(
                    Command::new("plasma-apply-wallpaperimage").arg(&path),
                    "plasma-apply-wallpaperimage",
                );
            }
            // 老版本 Plasma：qdbus 执行 js 设置
            let js = format!(
                "var d=desktops();for(var i=0;i<d.length;i++){{d[i].wallpaperPlugin='org.kde.image';d[i].currentConfigGroup=['Wallpaper','org.kde.image','General'];d[i].writeConfig('Image','{uri}');}}"
            );
            return run(
                Command::new("qdbus").args([
                    "org.kde.plasmashell",
                    "/PlasmaShell",
                    "org.kde.PlasmaShell.evaluateScript",
                    &js,
                ]),
                "qdbus plasmashell",
            );
        }
        // wlroots 系 / 其他：swww 优先，feh 兜底
        if which("swww") {
            return run(Command::new("swww").args(["img", &path]), "swww");
        }
        if which("feh") {
            return run(Command::new("feh").args(["--bg-fill", &path]), "feh");
        }
        Err(format!(
            "不支持的桌面环境（XDG_CURRENT_DESKTOP={desktops:?}）：系统壁纸同步已跳过"
        ))
    }
}

#[cfg(target_os = "linux")]
use imp::{extract_image_frame, extract_video_frame, set_all_screens};

// ---------- Windows：ffmpeg 抽帧 + SystemParametersInfo 设置 ----------

#[cfg(target_os = "windows")]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SPI_SETDESKWALLPAPER,
    };

    /// video/gif → 首帧 PNG。命令由 `crate::ffmpeg` 决定：应用内装的托管副本优先，
    /// 其次是 PATH 上的系统 ffmpeg。未安装时返回可操作的提示。
    fn ffmpeg_first_frame(input: &Path, out: &Path) -> Result<(), String> {
        let r = crate::ffmpeg::command()
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .arg("-i")
            .arg(input)
            .args(["-frames:v", "1"])
            .arg(out)
            .output();
        match r {
            Ok(o) if o.status.success() && out.is_file() => Ok(()),
            Ok(o) => Err(format!(
                "ffmpeg 抽帧失败: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(_) => Err(format!("未找到 ffmpeg：{}", crate::ffmpeg::MISSING_HINT)),
        }
    }

    pub fn extract_video_frame(input: &Path, out: &Path) -> Result<(), String> {
        ffmpeg_first_frame(input, out)
    }

    /// GIF 走 ffmpeg；其余交给通用路径
    pub fn extract_image_frame(input: &Path, out: &Path) -> Result<(), String> {
        ffmpeg_first_frame(input, out)
    }

    /// 设为桌面静态壁纸（SPI_SETDESKWALLPAPER）。
    ///
    /// 该系统调用是全局的：一次设置应用到所有显示器（Windows 的按屏壁纸要走
    /// IDesktopWallpaper COM，且只在 Win8+ 可用）—— 对「引擎未运行时保持视觉
    /// 一致」这个用途，全局同图已经达到目的，不额外引入 COM 依赖。
    ///
    /// 路径必须是绝对路径 + 反斜杠 + UTF-16 NUL 结尾；PNG/JPG 自 Windows 8 起
    /// 原生支持，所以我们写出的 PNG 可以直接用。
    pub fn set_all_screens(img: &Path) -> Result<(), String> {
        let abs = std::fs::canonicalize(img)
            .map_err(|e| format!("壁纸路径不可用（{}）: {e}", img.display()))?;
        // canonicalize 在 Windows 上返回 \\?\ 前缀的 verbatim 路径，
        // SystemParametersInfo 不认这个前缀，需要去掉
        let s = abs.to_string_lossy();
        let cleaned = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
        let mut wide: Vec<u16> = std::ffi::OsStr::new(&cleaned)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            SystemParametersInfoW(
                SPI_SETDESKWALLPAPER,
                0,
                Some(wide.as_mut_ptr() as *mut std::ffi::c_void),
                SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
            )
            .map_err(|e| format!("设置系统壁纸失败: {e}"))?;
        }
        Ok(())
    }

}

#[cfg(target_os = "windows")]
use imp::{extract_image_frame, extract_video_frame, set_all_screens};

// ---------- 其余平台：桩 ----------

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn extract_video_frame(_: &Path, _: &Path) -> Result<(), String> {
    Err("当前平台不支持".into())
}
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn extract_image_frame(_: &Path, _: &Path) -> Result<(), String> {
    Err("当前平台不支持".into())
}
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn set_all_screens(_: &Path) -> Result<(), String> {
    Err("当前平台不支持".into())
}

// ---------- 壁纸窗口截图（MCP 自检用） ----------

/// 当前渲染器的「ready」时间戳（epoch millis）。调用方在**应用壁纸之前**读一次，
/// 之后拿它等新一次 ready —— 否则可能立刻读到上一张壁纸留下的旧时间戳。
pub fn ready_stamp(app: &AppHandle) -> u64 {
    app.try_state::<crate::content_server::ContentServerState>()
        .map(|s| s.wallpaper_ready_ms.load(std::sync::atomic::Ordering::Relaxed))
        .unwrap_or(0)
}

/// 挑一面当前挂着壁纸的窗口 label（优先 scene/web：截图自检针对的就是它们）
// Linux 下 capture_wallpaper_png 走「不支持」分支，这个挑窗口的辅助函数只被 macOS 用
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn pick_wallpaper_window(app: &AppHandle) -> Option<String> {
    let engine = app.try_state::<crate::wallpaper::WallpaperEngineState>()?;
    let windows = engine.windows.lock().ok()?;
    windows
        .iter()
        .find(|(_, c)| matches!(c.r#type.as_str(), "scene" | "web"))
        .or_else(|| windows.iter().next())
        .map(|(label, _)| label.clone())
}

/// 等渲染器就绪 → 再等一拍 → 对壁纸窗口实拍 PNG。
///
/// `t0` 必须是应用壁纸**之前**读到的 `ready_stamp`；`settle_ms` 是首帧上屏后
/// 额外等待的时间（场景要几帧才稳定，截太早会拍到半成品）。
#[cfg(target_os = "macos")]
pub async fn capture_wallpaper_png(
    app: &AppHandle,
    t0: u64,
    settle_ms: u64,
    timeout_ms: u64,
) -> Result<Vec<u8>, String> {
    use std::sync::atomic::Ordering;

    let ready_ms = app
        .try_state::<crate::content_server::ContentServerState>()
        .ok_or("内容服务器未就绪")?
        .wallpaper_ready_ms
        .clone();

    let limit = timeout_ms.max(1000);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(limit);
    // 窗口在等待期间**反复探测**而不是开头探一次：应用新壁纸时会先建窗再挂载，
    // 建窗本身要几十到几百毫秒；开头探一次会在建窗完成前就报「没有壁纸窗口」。
    let mut label: Option<String> = None;
    loop {
        if label.is_none() {
            label = pick_wallpaper_window(app);
        }
        if ready_ms.load(Ordering::Relaxed) > t0 {
            break;
        }
        if std::time::Instant::now() >= deadline {
            // 超时原因必须可读：渲染器自报过失败就直接引用，否则给最近一条诊断
            // （常见是停在 "mount start"，说明大 scene.pkg 还在解析，重试或加大
            // timeoutMs 即可，而不是「壁纸页加载失败」）
            let hint = crate::content_server::renderer_diag_hint(app);
            return Err(match &label {
                Some(l) => format!(
                    "等待渲染器就绪超时（{limit}ms，窗口 {l}）。{hint}；可加大 timeoutMs 重试"
                ),
                None => format!("等待渲染器就绪超时（{limit}ms）：未找到正在显示的壁纸窗口。{hint}"),
            });
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let Some(label) = label.or_else(|| pick_wallpaper_window(app)) else {
        return Err("当前没有正在显示的壁纸窗口".into());
    };
    if settle_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(settle_ms)).await;
    }

    let window = crate::wallpaper::wallpaper_window(app, &label)
        .ok_or_else(|| format!("壁纸窗口 {label} 已不存在"))?;
    let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, String>>();
    window
        .with_webview(move |wv| imp::snapshot_webview(&wv, tx))
        .map_err(|e| format!("派发截图失败: {e}"))?;
    // WebContent 僵死时不能无限等：completion 走主线程回调
    tauri::async_runtime::spawn_blocking(move || {
        rx.recv_timeout(std::time::Duration::from_secs(20))
            .map_err(|_| "截图超时（WebContent 无响应）".to_string())?
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(not(target_os = "macos"))]
pub async fn capture_wallpaper_png(
    _app: &AppHandle,
    _t0: u64,
    _settle_ms: u64,
    _timeout_ms: u64,
) -> Result<Vec<u8>, String> {
    Err("壁纸截图自检目前仅支持 macOS（Linux/Windows 请用应用内预览或自行截图）".into())
}
