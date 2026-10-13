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
    let Ok(dir) = crate::library::item_dir(app, &item_id) else {
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
        // 等「新一次」ready：t0 之前的时间戳属于上一张壁纸。
        // 还要等换纸整体落地 —— 新的 ready 可能早于换纸任务收尾（现役窗还没
        // 交出控制权），光看时间戳会抢拍。
        // **事件驱动**：先订阅再查状态，渲染器报 ready / 换纸落地都会立刻唤醒
        // （见 content_server::EngineEvents）；睡觉只在等超时。
        let t0 = ready_ms.load(Ordering::Relaxed);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut events = crate::content_server::subscribe_engine(&app2);
        loop {
            if ready_ms.load(Ordering::Relaxed) > t0
                && !crate::wallpaper::any_swap_in_flight()
            {
                break;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                tracing::warn!("system wallpaper: 等渲染 ready 超时，跳过 {ty} 截图");
                return;
            }
            events.wait(deadline.saturating_duration_since(now)).await;
        }
        // ready 时首帧刚上屏；再等一拍让场景多渲染几帧，避免截到半成品画面
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        // 一屏一窗：按基 label 过解析层拿到那扇窗口
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

// ---------- KDE 静态壁纸三级回退链（平台无关的决策逻辑） ----------
//
// 为什么把这段放在 `imp` 外面：三级链的**决策**（试哪几级、谁先谁后、成功即停、
// 每级失败留什么日志、三级全败合成什么错误）是纯逻辑，必须能在**非 Linux 宿主**
// 上单测 —— 宿主 macOS 上 `#[cfg(target_os = "linux")] mod imp` 整个不参与编译，
// 逻辑写在里面就永远测不到。这里只有「决策」，真正的 D-Bus / 进程调用留在
// `imp::LinuxKdeWallpaper`（那是唯一被 `cfg(target_os = "linux")` 隔离的部分）。
//
// 回退顺序（Plasma 版本越新越靠前，上游 Internal API 压到最后兜底）：
//   1. portal `org.freedesktop.impl.portal.Wallpaper.SetWallpaper`（Plasma 6.5+，公开标准）
//   2. `plasma-apply-wallpaperimage` CLI（Plasma 5/6 都可能装）
//   3. `qdbus ... evaluateScript`（org.kde.plasmashell，上游 Internal API，仅兜底）
// 任一级成功即返回；三级全败时不静默吞掉，逐级 warn 后合成一条错误返回，
// 调用方 `sync_after_apply` 的容错行为（失败只记日志、不中断 apply 流程）保持不变。

/// 回退链的一级。顺序即尝试顺序 —— 单测直接断言这个顺序表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // 决策链唯一调用点在 Linux 分支
pub enum KdeFallbackStep {
    /// 一级：freedesktop portal（Plasma 6.5.0+）
    PortalWallpaper,
    /// 二级：`plasma-apply-wallpaperimage`
    PlasmaApplyWallpaperImage,
    /// 三级：`qdbus org.kde.plasmashell evaluateScript`（上游 Internal API）
    QdbusEvaluateScript,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl KdeFallbackStep {
    /// 声明式顺序表：**唯一**的事实来源，执行与日志/错误文案都由它推导。
    pub const ORDER: [KdeFallbackStep; 3] = [
        KdeFallbackStep::PortalWallpaper,
        KdeFallbackStep::PlasmaApplyWallpaperImage,
        KdeFallbackStep::QdbusEvaluateScript,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            KdeFallbackStep::PortalWallpaper => "portal",
            KdeFallbackStep::PlasmaApplyWallpaperImage => "plasma-apply-wallpaperimage",
            KdeFallbackStep::QdbusEvaluateScript => "qdbus-evaluatescript",
        }
    }

    /// 人话一级名，进日志与错误（真机取证时直接 grep 这一串）。
    pub fn label(self) -> &'static str {
        match self {
            KdeFallbackStep::PortalWallpaper => "portal Wallpaper.SetWallpaper",
            KdeFallbackStep::PlasmaApplyWallpaperImage => "plasma-apply-wallpaperimage",
            KdeFallbackStep::QdbusEvaluateScript => "qdbus evaluateScript",
        }
    }

    /// 一级失败时的日志文案模板。`{err}` 由调用点填具体错误。
    pub fn fail_msg_template(self) -> &'static str {
        match self {
            KdeFallbackStep::PortalWallpaper => {
                "system wallpaper: KDE 一级 portal Wallpaper.SetWallpaper 失败: {err}"
            }
            KdeFallbackStep::PlasmaApplyWallpaperImage => {
                "system wallpaper: KDE 二级 plasma-apply-wallpaperimage 失败: {err}"
            }
            KdeFallbackStep::QdbusEvaluateScript => {
                "system wallpaper: KDE 三级 qdbus evaluateScript 失败: {err}"
            }
        }
    }
}

/// 一次尝试的结果 + 该级的失败原因（成功时 `error` 为 `None`）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KdeAttempt {
    pub step: KdeFallbackStep,
    pub ok: bool,
    /// 失败原因。`is_available` 判定命令不存在时也是失败（原因写明未安装）。
    /// 三级全失败的错误信息只回填**真跑过**的级，没装的级不塞噪音。
    pub error: Option<String>,
}

/// 回退链结果：`attempts` 是**实际尝试过**的级（按序，含首个成功的级）；
/// 全失败时 `attempts` 长度即尝试次数，`error` 为合成错误。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KdeChainOutcome {
    pub ok: bool,
    pub attempts: Vec<KdeAttempt>,
    pub error: Option<String>,
}

/// 三级回退链要用的三件事。抽成 trait 是为了单测注入假实现 —— 决策逻辑
/// （顺序、短路、日志文案、合成错误）因此可以纯内存断言，**不起进程、不连 D-Bus**。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub trait KdeWallpaperTool {
    /// 命令存在性探测（真实现是 `sh -c command -v`）。
    fn is_available(&self, cmd: &str) -> bool;
    /// 一级：portal `SetWallpaper(file:// URI)`。
    fn attempt_portal(&self, _progress: &mut dyn FnMut(&str)) -> Result<(), String> {
        Err("portal 未实现".into())
    }
    /// 二级：`plasma-apply-wallpaperimage <path>`。
    fn attempt_plasma_apply(&self, _path: &str) -> Result<(), String> {
        Err("plasma-apply-wallpaperimage 未实现".into())
    }
    /// 三级：`qdbus org.kde.plasmashell evaluateScript <js>`。
    fn attempt_evaluate_script(&self, _js: &str) -> Result<(), String> {
        Err("qdbus evaluateScript 未实现".into())
    }
}

/// 三级全败时的合成错误：点名每一级与原因，调用方日志里一眼看出试过什么。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn kde_chain_error(attempts: &[KdeAttempt]) -> String {
    let detail = attempts
        .iter()
        .map(|a| {
            format!(
                "{}={}",
                a.step.label(),
                a.error.as_deref().unwrap_or("未知错误")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "KDE 静态壁纸三级回退链全部失败（已尝试 {} 级）: {}",
        attempts.len(),
        detail
    )
}

/// 跑一遍 KDE 静态壁纸回退链。纯决策，无平台依赖。
///
/// - 每级失败：`tracing::debug!` 记录「哪一级 + 什么错误」，继续下一级；
/// - `step` 可达性由 caller 决定（`PlasmaApplyWallpaperImage` 只在 `is_available` 为真时调用，
///   与旧行为一致：命令不存在时不再拿一次「command not found」当失败）；
/// - 三级全失败：`tracing::warn!` 记一条总结，返回 `Ok(KdeChainOutcome{ok:false,..})`
///   —— **不是** `Err`。错误语义留在 `error` 字段里，由调用方决定怎么呈现。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn run_kde_wallpaper_chain(
    js: &str,
    path: &str,
    progress: &mut dyn FnMut(&str),
    tool: &dyn KdeWallpaperTool,
) -> KdeChainOutcome {
    let mut attempts: Vec<KdeAttempt> = Vec::new();
    progress("kde: 静态壁纸回退链开始（portal → plasma-apply-wallpaperimage → qdbus）");

    for step in KdeFallbackStep::ORDER {
        let result = match step {
            KdeFallbackStep::PortalWallpaper => tool.attempt_portal(progress),
            KdeFallbackStep::PlasmaApplyWallpaperImage => {
                if !tool.is_available("plasma-apply-wallpaperimage") {
                    // 命令不存在不算「试过」：跳过不记尝试，与旧行为（which 为假直接下一路）一致
                    progress("kde: 未找到 plasma-apply-wallpaperimage，跳过二级");
                    continue;
                }
                tool.attempt_plasma_apply(path)
            }
            KdeFallbackStep::QdbusEvaluateScript => tool.attempt_evaluate_script(js),
        };

        match result {
            Ok(()) => {
                attempts.push(KdeAttempt {
                    step,
                    ok: true,
                    error: None,
                });
                tracing::info!(
                    "system wallpaper: KDE 回退链在第 {} 级（{}）成功",
                    attempts.len(),
                    step.label()
                );
                progress(&format!(
                    "kde: 回退链成功于 {}（第 {} 级）",
                    step.label(),
                    attempts.len()
                ));
                return KdeChainOutcome {
                    ok: true,
                    attempts,
                    error: None,
                };
            }
            Err(e) => {
                tracing::debug!("{}", step.fail_msg_template().replace("{err}", &e));
                progress(&format!(
                    "kde: 回退链 {} 失败，继续下一级: {e}",
                    step.label()
                ));
                attempts.push(KdeAttempt {
                    step,
                    ok: false,
                    error: Some(e),
                });
            }
        }
    }

    let error = kde_chain_error(&attempts);
    tracing::warn!(
        "system wallpaper: {error}；本次系统静态壁纸同步已跳过（不影响壁纸应用流程）"
    );
    KdeChainOutcome {
        ok: false,
        attempts,
        error: Some(error),
    }
}

/// KDE 分支的 JS 脚本（三级里 qdbus 那级用）。抽成函数是为了让单测能直接断言
/// 脚本里确实写了 `org.kde.image` / `Image` —— 这段 JS 是历史包袱（上游 Internal API），
/// 合错了会静默把壁纸设成纯色而不报错。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn kde_evaluate_script(uri: &str) -> String {
    format!(
        "var d=desktops();for(var i=0;i<d.length;i++){{d[i].wallpaperPlugin='org.kde.image';d[i].currentConfigGroup=['Wallpaper','org.kde.image','General'];d[i].writeConfig('Image','{uri}');}}"
    )
}

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

    /// 回退链的真实现：唯一被 `cfg(target_os = "linux")` 隔离的部分（进程 / D-Bus）。
    /// 决策逻辑在平台无关的 [`super::run_kde_wallpaper_chain`]，宿主 macOS 上可单测。
    ///
    /// 一级为什么用 `zbus::blocking` 而不是 `gdbus` CLI 子进程：
    /// - zbus 已在 Linux 依赖里（`Cargo.toml` 的 linux target，kde.rs 也在用），
    ///   `blocking` 走 async-io 的 `block_on`，**借不借 tokio 运行时都能跑**；
    /// - `gdbus` 属于 glib2 附带工具，精简发行版 / Flatpak 里未必有，拿它当一级
    ///   等于把「portal 可用」这个判断建立在另一个外部命令上；
    /// - 子进程还要处理 stdout 解析与退出码，错误信息更糊。
    /// 代价是 zbus 首次连接要几百毫秒，故整级包 1500ms 超时（见 [`portal_set_wallpaper`]）。
    struct LinuxKdeWallpaper {
        /// 一级 portal 要的 `file://` URI（portal 只吃 URI，不吃路径）。
        uri: String,
    }

    impl super::KdeWallpaperTool for LinuxKdeWallpaper {
        fn is_available(&self, cmd: &str) -> bool {
            which(cmd)
        }

        fn attempt_portal(&self, progress: &mut dyn FnMut(&str)) -> Result<(), String> {
            // 老版本 Plasma 上没有 portal 后端：先看服务名在不在，省一次必然的超时。
            if !portal_service_present() {
                return Err("会话总线上没有 org.freedesktop.portal.Desktop".into());
            }
            progress("kde: 一级 portal Wallpaper.SetWallpaper 开始");
            portal_set_wallpaper(&self.uri)
        }

        fn attempt_plasma_apply(&self, path: &str) -> Result<(), String> {
            run(
                Command::new("plasma-apply-wallpaperimage").arg(path),
                "plasma-apply-wallpaperimage",
            )
        }

        fn attempt_evaluate_script(&self, js: &str) -> Result<(), String> {
            run(
                Command::new("qdbus").args([
                    "org.kde.plasmashell",
                    "/PlasmaShell",
                    "org.kde.PlasmaShell.evaluateScript",
                    js,
                ]),
                "qdbus plasmashell",
            )
        }
    }

    /// portal 服务名是否在会话总线上（`NameHasOwner`，250ms 超时；任何失败都当「不在」）。
    fn portal_service_present() -> bool {
        use std::time::Duration;
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("kde-portal-probe".into())
            .spawn(move || {
                let ok = (|| {
                    let conn = zbus::blocking::Connection::session().ok()?;
                    let reply = conn
                        .call_method(
                            Some("org.freedesktop.DBus"),
                            "/org/freedesktop/DBus",
                            Some("org.freedesktop.DBus"),
                            "NameHasOwner",
                            &("org.freedesktop.portal.Desktop",),
                        )
                        .ok()?;
                    reply.body().deserialize::<bool>().ok()
                })()
                .unwrap_or(false);
                let _ = tx.send(ok);
            });
        rx.recv_timeout(Duration::from_millis(250)).unwrap_or(false)
    }

    /// 一级：`org.freedesktop.impl.portal.Wallpaper.SetWallpaper(uri, showPreview, options)`
    /// （`org.freedesktop.portal.Desktop` 的 `/org/freedesktop/portal/desktop`）。
    ///
    /// 只吃 `file://` URI，不接受路径；`options` 是 `a{sv}`，带一个 handle_token 便于
    /// 真机取证时和 portal 日志对齐。返回的 object path **不代表已生效**，这里只把它
    /// 当「调用被接受」；真正的失败（后端不支持）会以 D-Bus 错误回来。
    ///
    /// 整级 1500ms 超时：portal 后端缺失时调用可能长时间无响应，而本函数在 apply 流程
    /// 的收尾路径上，不能把主线程吊死在一个「锦上添花」的静态壁纸上。超时后线程自然结束
    /// （进程退出即回收），只是不再等它的结果。
    fn portal_set_wallpaper(uri: &str) -> Result<(), String> {
        use std::collections::HashMap;
        use std::time::Duration;

        let mut options: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
        options.insert(
            "handle_token",
            zbus::zvariant::Value::from("wallpaperem_sync"),
        );
        // 调用要挪进子线程（超时用），所以 URI 必须先转 owned —— `&str` 活不到 'static
        let uri: String = uri.to_string();

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("kde-portal-wallpaper".into())
            .spawn(move || {
                let call = (|| -> Result<String, String> {
                    let conn = zbus::blocking::Connection::session()
                        .map_err(|e| format!("会话总线连接失败: {e}"))?;
                    let reply = conn
                        .call_method(
                            Some("org.freedesktop.portal.Desktop"),
                            "/org/freedesktop/portal/desktop",
                            Some("org.freedesktop.impl.portal.Wallpaper"),
                            "SetWallpaper",
                            &(uri.as_str(), false, &options),
                        )
                        .map_err(|e| format!("SetWallpaper 调用失败: {e}"))?;
                    reply
                        .body()
                        .deserialize::<zbus::zvariant::OwnedObjectPath>()
                        .map(|p| p.as_str().to_string())
                        .map_err(|e| format!("SetWallpaper 返回值无法解析: {e}"))
                })();
                let _ = tx.send(call);
            })
            .map_err(|e| format!("portal 调用线程启动失败: {e}"))?;

        match rx.recv_timeout(Duration::from_millis(1500)) {
            Ok(Ok(_handle)) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("portal SetWallpaper 超时（1500ms）".into()),
        }
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
        if crate::kde::env().is_kde {
            // KDE 判定统一走 crate::kde（冒号列表 / 大小写 / KDE_FULL_SESSION 都已处理）：
            // 旧代码在这里自己大写比较 `XDG_CURRENT_DESKTOP`，历史值 `KDE:Plasma` 之外
            // 的写法会漏判（见 src-tauri/src/kde.rs 顶部两条硬规则）。
            //
            // 三级回退链（portal → plasma-apply-wallpaperimage → qdbus evaluateScript）：
            // 旧实现两级都 `return`，第一级失败就直接放弃同步静态壁纸 —— 于是 Plasma 6.5+
            // 上 portal 可用、或只有 qdbus 可用的机器永远设不上。现在逐级降级。
            let tool = LinuxKdeWallpaper { uri: uri.clone() };
            let js = kde_evaluate_script(&uri);
            let outcome = run_kde_wallpaper_chain(&js, &path, &mut |m| tracing::debug!("{m}"), &tool);
            return match outcome.error {
                None => Ok(()),
                // 三级全失败：错误原样上抛给 sync_after_apply，由它决定只记 warn
                // （容错语义与旧实现一致：静态壁纸同步失败绝不中断 apply 流程）。
                Some(e) => Err(e),
            };
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

/// 某块屏的壁纸窗口 label（`wallpaper-<displayId>`，见 wallpaper 模块的建窗命名）
pub fn label_for_display(display_id: &str) -> String {
    format!("wallpaper-{}", display_id.trim())
}

/// 屏幕上当前挂着哪一条壁纸：返回 `(窗口 label, 条目 id)`。
///
/// `display_id` 给定时只看那块屏（多屏各挂不同壁纸时，截图必须能指定拍哪一块）；
/// 缺省优先 scene/web 那扇（截图自检针对的就是它们），否则取第一扇。
///
/// 截图工具用它做**一致性校验**：`apply=false` 表示「拍屏上现在这张」，那么屏上那张
/// 必须就是调用方要的那张 —— 否则会把 A 的画面当成 B 交出去（比报错难查得多）。
pub fn onscreen_item(app: &AppHandle, display_id: Option<&str>) -> Option<(String, String)> {
    let want = display_id.map(str::trim).filter(|d| !d.is_empty());
    let label = match want {
        Some(d) => label_for_display(d),
        None => {
            // 没点名屏：优先 scene/web 那扇（截图自检针对的就是它们）
            let engine = app.try_state::<crate::wallpaper::WallpaperEngineState>()?;
            let windows = engine.windows.lock().ok()?;
            windows
                .iter()
                .find(|(_, c)| matches!(c.r#type.as_str(), "scene" | "web"))
                .or_else(|| windows.iter().next())
                .map(|(l, _)| l.clone())?
        }
    };
    // 条目 id：内存会话优先，**DB 兜底** —— 重启后窗口可能由恢复流程「接管」而不是
    // 重建，内存 map 未必有它（曾因此让 apply=false 的一致性校验静默失效：查不到就跳过
    // 校验，于是把屏上别的壁纸当成请求的那张交了出去）。
    let from_mem = app
        .try_state::<crate::wallpaper::WallpaperEngineState>()
        .and_then(|st| {
            let windows = st.windows.lock().ok()?;
            windows.get(&label).and_then(crate::wallpaper::item_id_of)
        });
    let id = from_mem.or_else(|| {
        let db = app.try_state::<std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>>()?;
        let conn = db.lock().ok()?;
        conn.query_row(
            "SELECT item_id FROM wallpaper_sessions WHERE display_id = ?1",
            [&label.trim_start_matches("wallpaper-")],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    });
    // 窗口不存在就当作「屏上什么都没有」：调用方据此报错，而不是拍一张别的东西
    app.get_webview_window(&label)?;
    Some((label, id.unwrap_or_default()))
}

/// 挑一面当前挂着壁纸的窗口 label（优先 scene/web：截图自检针对的就是它们）
// Linux 下 capture_wallpaper_png 走「不支持」分支，这个挑窗口的辅助函数只被 macOS 用
fn pick_wallpaper_window(app: &AppHandle) -> Option<String> {
    let engine = app.try_state::<crate::wallpaper::WallpaperEngineState>()?;
    let windows = engine.windows.lock().ok()?;
    windows
        .iter()
        .find(|(_, c)| matches!(c.r#type.as_str(), "scene" | "web"))
        .or_else(|| windows.iter().next())
        .map(|(label, _)| label.clone())
}

/// 等渲染器就绪 → 再等一拍，返回**该拍哪扇窗**（label）。
///
/// 跨平台共用：三条截图路（页面抓帧 / macOS 原生快照 / 未来的平台实现）都要先等
/// 「这一张壁纸真的画完了」。`t0` 必须是应用壁纸**之前**读到的 `ready_stamp`；
/// `settle_ms` 是首帧上屏后额外等待的时间（场景要几帧才稳定，截太早会拍到半成品）。
pub async fn wait_renderer_ready(
    app: &AppHandle,
    t0: u64,
    settle_ms: u64,
    timeout_ms: u64,
    only_label: Option<&str>,
) -> Result<String, String> {
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
    // **事件驱动**：订阅引擎事件，渲染器报 ready / 换纸落地即醒；睡觉只为超时。
    let mut events = crate::content_server::subscribe_engine(app);
    // 指定了 label（多屏时点名拍哪一块屏）就只认它
    let fixed = only_label.map(|l| l.to_string());
    let mut label: Option<String> = fixed.clone();
    loop {
        if label.is_none() {
            label = pick_wallpaper_window(app);
        }
        // 别拍在换纸半途：ready 可能早于换纸任务收尾，单飞集合腾空才算「画面真的换完了」
        if ready_ms.load(Ordering::Relaxed) > t0
            && !crate::wallpaper::any_swap_in_flight()
        {
            break;
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            // 超时原因必须可读：渲染器自报过失败就直接引用，否则给最近一条诊断
            // （常见是停在 "mount start"，说明大 scene.pkg 还在解析，重试或加大
            // timeoutMs 即可，而不是「壁纸页加载失败」）
            let hint = crate::content_server::renderer_diag_hint(app);
            return Err(match &label {
                Some(l) => format!(
                    "等待渲染器就绪超时（{limit}ms，窗口 {l}）。{hint}；可加大 timeoutMs 重试"
                ),
                None if fixed.is_some() => format!(
                    "等待渲染器就绪超时（{limit}ms）：指定屏的壁纸窗口 {} 不存在（该屏没挂壁纸？用 displays_list 核对 id）。{hint}",
                    fixed.clone().unwrap_or_default()
                ),
                None => format!("等待渲染器就绪超时（{limit}ms）：未找到正在显示的壁纸窗口。{hint}"),
            });
        }
        events.wait(deadline.saturating_duration_since(now)).await;
    }
    let Some(label) = label.or_else(|| pick_wallpaper_window(app)) else {
        return Err("当前没有正在显示的壁纸窗口".into());
    };
    if settle_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(settle_ms)).await;
    }
    Ok(label)
}

/// **页面自抓帧**（三个平台同一套，见 [`crate::content_server::request_capture`]）。
///
/// 覆盖 scene / gif / image / canvas / video（视频帧与页面同源，drawImage 可用）；
/// 只有 web 类型（独立 iframe）抓不到 —— 那时返回的错误信息里写明了原因，调用方
/// 可回退到平台原生快照。
pub async fn capture_via_renderer(
    app: &AppHandle,
    t0: u64,
    settle_ms: u64,
    timeout_ms: u64,
    max_width: Option<u32>,
    only_label: Option<&str>,
) -> Result<(Vec<u8>, String, String), String> {
    let label = wait_renderer_ready(app, t0, settle_ms, timeout_ms, only_label).await?;
    // 抓帧本身给 20s：页面里是一次 drawImage + toDataURL + 一次 POST
    let (bytes, mime) = crate::content_server::request_capture(
        app,
        &label,
        max_width,
        std::time::Duration::from_secs(20),
    )
    .await?;
    Ok((bytes, mime, label))
}

/// 等渲染器就绪 → 再等一拍 → 对壁纸窗口实拍 PNG（macOS 原生快照，保真度最高：
/// 视频/网页类型的合成结果都在里面）。
#[cfg(target_os = "macos")]
pub async fn capture_wallpaper_png(
    app: &AppHandle,
    t0: u64,
    settle_ms: u64,
    timeout_ms: u64,
    only_label: Option<&str>,
) -> Result<Vec<u8>, String> {
    let label = wait_renderer_ready(app, t0, settle_ms, timeout_ms, only_label).await?;

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
    _only_label: Option<&str>,
) -> Result<Vec<u8>, String> {
    // 只有 web 类型会走到这里（页面抓帧抓不到 iframe），且该平台没有原生快照实现。
    //
    // 要补齐这一步需要各平台的原生抓帧：Linux 走 WebKitGTK 的
    // `webkit_web_view_get_snapshot()`（配 `webkit2gtk` crate），Windows 走 WebView2 的
    // `ICoreWebView2CapturePreview`（配 `webview2-com`）。两者都需要在对应平台上编译与
    // 实机验证，本仓的 CI 只出 macOS 产物 —— 与其盲写两份编不过的代码，不如在这里
    // 明确说清代价与替代路径（见 docs/mcp-authoring-scene.md §11）。
    Err("这个平台没有原生窗口快照（仅 macOS 有）；web 类型请用应用内预览，\
         或改用 scene / video / gif / image 类型（那几类走渲染器自抓帧，三平台都能截）"
        .into())
}

// ---------- KDE 回退链单测（平台无关；宿主 macOS / Linux / Windows 都可跑） ----------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// 假实现：按剧本逐级返回成功/失败，并把**调用顺序**记下来。
    /// 不碰进程、不连 D-Bus，所以整套失败矩阵在宿主 macOS 上就能跑。
    struct FakeKde {
        /// 命令存在性探测的回答（默认 plasma-apply 与 qdbus 都在）
        installed: Vec<String>,
        portal: Result<(), String>,
        plasma: Result<(), String>,
        qdbus: Result<(), String>,
        /// 记录每次真实调用：「is_available:<cmd>」/「portal:<uri>」/「plasma:<path>」/「qdbus:<js>」
        log: RefCell<Vec<String>>,
    }

    impl FakeKde {
        fn all_fail() -> Self {
            Self {
                installed: vec!["plasma-apply-wallpaperimage".into(), "qdbus".into()],
                portal: Err("PortalNotFound: 后端不支持".into()),
                plasma: Err("plasma-apply-wallpaperimage: 退出码 1".into()),
                qdbus: Err("qdbus plasmashell: Could not connect to D-Bus".into()),
                log: RefCell::new(Vec::new()),
            }
        }
        fn record(&self, entry: String) {
            self.log.borrow_mut().push(entry);
        }
        fn calls(&self) -> Vec<String> {
            self.log.borrow().clone()
        }
    }

    impl KdeWallpaperTool for FakeKde {
        fn is_available(&self, cmd: &str) -> bool {
            self.record(format!("is_available:{cmd}"));
            self.installed.iter().any(|c| c == cmd)
        }
        fn attempt_portal(&self, _progress: &mut dyn FnMut(&str)) -> Result<(), String> {
            self.record("portal".into());
            self.portal.clone()
        }
        fn attempt_plasma_apply(&self, path: &str) -> Result<(), String> {
            self.record(format!("plasma:{path}"));
            self.plasma.clone()
        }
        fn attempt_evaluate_script(&self, js: &str) -> Result<(), String> {
            self.record(format!("qdbus:{js}"));
            self.qdbus.clone()
        }
    }

    const IMG: &str = "/tmp/wall wallpaper/frame.png";

    fn run(tool: &FakeKde) -> KdeChainOutcome {
        let js = kde_evaluate_script("file:///tmp/wall%20wallpaper/frame.png");
        // progress 走闭包收集，不接 tracing（单测不该依赖 subscriber）
        let mut notes: Vec<String> = Vec::new();
        let out = run_kde_wallpaper_chain(&js, IMG, &mut |m| notes.push(m.to_string()), tool);
        assert!(!notes.is_empty(), "回退链必须留下 progress 轨迹");
        out
    }

    // ---------- 顺序表 ----------

    #[test]
    fn chain_order_is_portal_then_cli_then_qdbus() {
        assert_eq!(
            KdeFallbackStep::ORDER,
            [
                KdeFallbackStep::PortalWallpaper,
                KdeFallbackStep::PlasmaApplyWallpaperImage,
                KdeFallbackStep::QdbusEvaluateScript,
            ],
            "回退顺序硬约束：portal → plasma-apply-wallpaperimage → qdbus"
        );
    }

    // ---------- 矩阵：谁先成功 ----------

    #[test]
    fn portal_success_short_circuits_rest() {
        let tool = FakeKde {
            portal: Ok(()),
            ..FakeKde::all_fail()
        };
        let out = run(&tool);
        assert!(out.ok);
        assert_eq!(out.attempts.len(), 1);
        assert_eq!(out.attempts[0].step, KdeFallbackStep::PortalWallpaper);
        assert!(out.error.is_none());
        // 一级成功即返回：二级的命令探测与三级都不许发生
        assert_eq!(tool.calls(), vec!["portal".to_string()]);
    }

    #[test]
    fn portal_fails_then_cli_succeeds() {
        let tool = FakeKde {
            plasma: Ok(()),
            ..FakeKde::all_fail()
        };
        let out = run(&tool);
        assert!(out.ok, "一级失败必须降级到二级，而不是像旧代码那样直接放弃");
        assert_eq!(
            out.attempts.iter().map(|a| a.step).collect::<Vec<_>>(),
            vec![
                KdeFallbackStep::PortalWallpaper,
                KdeFallbackStep::PlasmaApplyWallpaperImage
            ]
        );
        assert!(out.attempts[0].error.is_some(), "失败的那级要留下原因");
        assert_eq!(
            tool.calls(),
            vec![
                "portal".to_string(),
                "is_available:plasma-apply-wallpaperimage".to_string(),
                format!("plasma:{IMG}"),
            ],
            "二级成功则三级（qdbus）不应再尝试"
        );
    }

    #[test]
    fn portal_and_cli_fail_then_qdbus_succeeds() {
        let tool = FakeKde {
            qdbus: Ok(()),
            ..FakeKde::all_fail()
        };
        let out = run(&tool);
        assert!(out.ok);
        assert_eq!(out.attempts.len(), 3);
        assert_eq!(
            out.attempts.last().unwrap().step,
            KdeFallbackStep::QdbusEvaluateScript
        );
        let calls = tool.calls();
        assert_eq!(calls.first().unwrap(), "portal");
        assert_eq!(
            calls.last().unwrap().split(':').next().unwrap(),
            "qdbus",
            "三级兜底必须真的执行 evaluateScript"
        );
        // JS 里必须是 org.kde.image + writeConfig('Image', ...)，写错了会静默设成纯色
        assert!(calls.last().unwrap().contains("org.kde.image"));
        assert!(calls.last().unwrap().contains("writeConfig('Image'"));
        assert!(calls.last().unwrap().contains("file:///tmp/wall%20wallpaper/frame.png"));
    }

    // ---------- 全失败 / 命令不存在 ----------

    #[test]
    fn all_three_fail_returns_skipped_with_full_diagnostics() {
        let tool = FakeKde::all_fail();
        let out = run(&tool);
        assert!(!out.ok, "三级全失败不能报成功");
        assert_eq!(out.attempts.len(), 3, "三级都试过");
        assert!(out.attempts.iter().all(|a| !a.ok && a.error.is_some()));

        let err = out.error.clone().expect("全失败必须带错误信息");
        // 错误里点名每一级与原因 —— 真机取证时一眼看出试过什么
        assert!(err.contains("三级回退链全部失败"), "{err}");
        assert!(err.contains("已尝试 3 级"), "{err}");
        assert!(err.contains("portal Wallpaper.SetWallpaper"), "{err}");
        assert!(err.contains("plasma-apply-wallpaperimage"), "{err}");
        assert!(err.contains("qdbus evaluateScript"), "{err}");
        assert!(err.contains("PortalNotFound"), "{err}");
        assert!(err.contains("Could not connect to D-Bus"), "{err}");
    }

    #[test]
    fn missing_cli_is_skipped_not_counted_as_an_attempt() {
        // 只装了 qdbus：plasma-apply-wallpaperimage 不存在
        let tool = FakeKde {
            installed: vec!["qdbus".into()],
            qdbus: Ok(()),
            ..FakeKde::all_fail()
        };
        let out = run(&tool);
        assert!(out.ok);
        assert_eq!(
            out.attempts.iter().map(|a| a.step).collect::<Vec<_>>(),
            vec![
                KdeFallbackStep::PortalWallpaper,
                KdeFallbackStep::QdbusEvaluateScript
            ],
            "命令不存在的级不算尝试（旧行为：which 为假直接下一路）"
        );
    }

    #[test]
    fn missing_cli_and_qdbus_failure_reports_only_real_failures() {
        let tool = FakeKde {
            installed: vec![], // plasma-apply 与 qdbus 都没装
            ..FakeKde::all_fail()
        };
        let out = run(&tool);
        assert!(!out.ok);
        // 只有 portal 真跑过；plasma 级被跳过
        assert_eq!(out.attempts.len(), 2);
        let err = out.error.unwrap();
        assert!(err.contains("已尝试 2 级"), "{err}");
        assert!(!err.contains("plasma-apply-wallpaperimage="), "{err}");
    }

    // ---------- 日志文案模板 ----------

    #[test]
    fn fail_log_templates_name_the_level_and_hold_the_error() {
        let cases = [
            (
                KdeFallbackStep::PortalWallpaper,
                "一级 portal Wallpaper.SetWallpaper",
            ),
            (
                KdeFallbackStep::PlasmaApplyWallpaperImage,
                "二级 plasma-apply-wallpaperimage",
            ),
            (
                KdeFallbackStep::QdbusEvaluateScript,
                "三级 qdbus evaluateScript",
            ),
        ];
        for (step, want) in cases {
            let msg = step.fail_msg_template().replace("{err}", "BOOM");
            assert!(msg.starts_with("system wallpaper: KDE "), "{msg}");
            assert!(msg.contains(want), "{msg} 应点名是哪一级");
            assert!(msg.ends_with(": BOOM"), "{msg} 应带上具体错误");
        }
    }

    // ---------- JS 生成（历史包袱，静默失败风险点） ----------

    #[test]
    fn evaluate_script_targets_image_plugin() {
        let js = kde_evaluate_script("file:///a%20b.png");
        assert!(js.contains("d[i].wallpaperPlugin='org.kde.image'"), "{js}");
        assert!(
            js.contains("currentConfigGroup=['Wallpaper','org.kde.image','General']"),
            "{js}"
        );
        assert!(js.contains("writeConfig('Image','file:///a%20b.png')"), "{js}");
    }
}
