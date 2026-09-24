//! 封面本地化：本地库卡片优先用壁纸目录里的 preview.jpg/preview.gif，
//! 目录里没有时才去远端（工坊元数据缓存的 preview_url）取一份写回目录。
//!
//! 为什么落盘而不是一直挂远端 URL：
//! - 离线可用：Steam CDN 不通时卡片照样有图
//! - 卡片渲染不再依赖第三方域名，库页也不再一进去就打一批跨网请求
//!
//! 两条硬约束（改动时别丢）：
//! - **绝不覆盖**目录里已有的 preview.*（WE 工程自带的 preview.gif、用户手放的
//!   preview.jpg 都是作者/用户的意图，不是我们可以替换的缓存）
//! - 文件名必须与内容格式一致（按魔数认，不信 URL 扩展名/Content-Type）。
//!   历史 bug 就在这儿：同一源同时写成 preview.gif + preview.png，扩展名与
//!   内容对不上，本地库卡片随机裂图
//!
//! 网络请求跑在后台（见 `enqueue`），列表路径只入队，永远不等封面。

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager};

use crate::library::PREVIEW_EXTS;

/// 单张封面的体积上限：正常封面几百 KB（动图 gif 也就几 MB），
/// 到这个量级说明拿到的不是封面或远端行为异常，直接放弃
const MAX_COVER_BYTES: u64 = 16 << 20;

/// JPEG 质量：封面就是卡片上的一张小图，90 足够且体积可控
const JPEG_QUALITY: u8 = 90;

const COVER_JPG: &str = "preview.jpg";
const COVER_GIF: &str = "preview.gif";
/// 动图 webp 是「统一成 jpg/gif」的唯一例外：静态化会丢动画，
/// 保留原格式（文件名与内容仍然一致，local_preview_url 也认 webp）
const COVER_WEBP: &str = "preview.webp";

/// 落盘中途的临时名：先写它再 rename，避免内容服务器/卡片读到半截文件。
/// 带点前缀且不匹配 preview.<ext>，不会被 has_preview_file 之类误认
const PART_NAME: &str = ".preview.part";

/// 一条待补封面的条目
pub(crate) struct CoverJob {
    pub(crate) item_id: String,
    /// 条目内容目录（引用模式条目是源目录）
    pub(crate) dir: PathBuf,
    /// 工坊元数据里的远端封面 URL
    pub(crate) url: String,
}

/// 封面补齐队列。
/// `seen` 只存内存：同一次运行里同一张封面不重复尝试（失败也不重试，
/// 与 `library::PosterFailState` 同一套记账思路），重启后会再试一次。
#[derive(Default)]
pub struct CoverCacheState {
    seen: Mutex<HashSet<String>>,
    queue: Mutex<VecDeque<CoverJob>>,
    running: AtomicBool,
}

/// 把缺本地封面的条目排进后台补齐队列。
pub(crate) fn enqueue(app: &AppHandle, jobs: Vec<CoverJob>) {
    if jobs.is_empty() {
        return;
    }
    // 没有 Steam 客户端（代理配置异常等）就没得下：这次不占用 seen，
    // 等客户端就绪后下一次列表刷新还能补上
    if app.try_state::<crate::steam::SteamClient>().is_none() {
        return;
    }
    let Some(state) = app.try_state::<Arc<CoverCacheState>>() else {
        return;
    };
    {
        let (Ok(mut seen), Ok(mut queue)) = (state.seen.lock(), state.queue.lock()) else {
            return;
        };
        for job in jobs {
            // 只认 http(s)：别的 scheme 交给 reqwest 也只会报错
            if !job.url.starts_with("http") {
                continue;
            }
            if seen.insert(job.item_id.clone()) {
                queue.push_back(job);
            }
        }
        if queue.is_empty() {
            return;
        }
    }
    start_worker(app, state.inner().clone());
}

/// 起一个 worker 串行搬队列（封面都很小，串行对 CDN 也更友好）
fn start_worker(app: &AppHandle, state: Arc<CoverCacheState>) {
    if state.running.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move { drain(app, state).await });
}

async fn drain(app: AppHandle, state: Arc<CoverCacheState>) {
    loop {
        let job = state.queue.lock().ok().and_then(|mut q| q.pop_front());
        let Some(job) = job else {
            // 退出前落回空闲位。置位与「队列已空」判定之间可能刚有新任务入队
            // （enqueue 看到 running=true 就不再起 worker），所以清空后再复核一次，
            // 否则它们会一直躺在队列里没人搬
            state.running.store(false, Ordering::SeqCst);
            let empty = state.queue.lock().map(|q| q.is_empty()).unwrap_or(true);
            if empty || state.running.swap(true, Ordering::SeqCst) {
                break;
            }
            continue;
        };
        let id = job.item_id.clone();
        // 入队到干活之间目录里可能已经有封面了（并发的前一次补齐、用户手放），
        // 先看一眼再决定日志口径：只有真下了网络才值得记 info
        let already = existing_cover(&job.dir).is_some();
        match fetch_and_store(&app, &job).await {
            Ok(_) if already => tracing::debug!("封面已在本地，无需补齐: {id}"),
            Ok(p) => tracing::info!("封面已补到本地: {id} → {}", p.display()),
            Err(e) => tracing::warn!("封面补齐失败（本次运行不再重试）{id}: {e}"),
        }
    }
}

/// 下载一张封面并写进条目目录。
async fn fetch_and_store(app: &AppHandle, job: &CoverJob) -> Result<PathBuf, String> {
    let client = app
        .try_state::<crate::steam::SteamClient>()
        .ok_or("Steam 客户端未就绪")?;
    fetch_into(client.http(), &job.dir, &job.url, MAX_COVER_BYTES).await
}

/// 下载 + 落盘（封面本地化的全部实际动作）。
///
/// client 与体积上限都由调用方给：生产路径用 Steam 客户端的 http（带代理/UA 配置）
/// 与 `MAX_COVER_BYTES`，单测用小上限的本地 mock 服务验证越界即中断。
async fn fetch_into(
    client: &reqwest::Client,
    dir: &Path,
    url: &str,
    cap: u64,
) -> Result<PathBuf, String> {
    if !dir.is_dir() {
        return Err("壁纸目录不存在".into());
    }
    // 目录里已经有封面（用户手放 / WE 工程自带 / 上一次补过）→ 一个字都不动，
    // 也不再发这次请求
    if let Some(existing) = existing_cover(dir) {
        return Ok(existing);
    }
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("远端返回 {}", resp.status()));
    }
    if resp.content_length().unwrap_or(0) > cap {
        return Err("远端封面超过体积上限".into());
    }
    // 边收边卡上限：只看 Content-Length 挡不住不报长度（chunked）的响应
    let mut resp = resp;
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("读取失败: {e}"))? {
        if bytes.len() as u64 + chunk.len() as u64 > cap {
            return Err("远端封面超过体积上限".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    store_cover(dir, &bytes)
}

/// 目录里已有的封面文件（**任何** preview.<ext> 都算，不挑格式 —— 有就不动它）
pub(crate) fn existing_cover(dir: &Path) -> Option<PathBuf> {
    PREVIEW_EXTS
        .iter()
        .map(|e| dir.join(format!("preview.{e}")))
        .find(|p| p.is_file())
}

/// 把封面字节写进目录，返回落盘路径。
///
/// gif / jpeg 原样存（再编码一次只会掉画质），png 与静态 webp 转成 jpg。
pub(crate) fn store_cover(dir: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let Some(kind) = sniff(bytes) else {
        return Err("远端返回的不是图片".into());
    };
    let name = match kind {
        ImageKind::Gif => COVER_GIF,
        ImageKind::Jpeg | ImageKind::Png => COVER_JPG,
        ImageKind::Webp if is_animated_webp(bytes) => COVER_WEBP,
        ImageKind::Webp => COVER_JPG,
    };
    let data = if kind != ImageKind::Jpeg && name == COVER_JPG {
        to_jpeg(bytes)?
    } else {
        bytes.to_vec()
    };
    let tmp = dir.join(PART_NAME);
    std::fs::write(&tmp, &data).map_err(|e| format!("写封面失败: {e}"))?;
    let out = dir.join(name);
    std::fs::rename(&tmp, &out).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("落盘封面失败: {e}")
    })?;
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageKind {
    Gif,
    Jpeg,
    Png,
    Webp,
}

/// 按魔数认格式：远端 URL 的扩展名和 Content-Type 都不可信
/// （拿某个扩展名的地址吐另一种格式是家常便饭），认错了就会写出
/// 「扩展名与内容对不上」的封面 —— 本地库卡片会因此裂图
fn sniff(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(ImageKind::Gif);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(ImageKind::Jpeg);
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(ImageKind::Png);
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(ImageKind::Webp);
    }
    None
}

/// webp 是否带动画（RIFF 里的 ANIM 块）。线性扫一次就够：4 字节 ASCII 块名
/// 在真实文件里不会偶然出现，误判的代价也只是「本该转 jpg 的留成了 webp」，
/// 文件名与内容依然一致，不会裂图
fn is_animated_webp(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|w| w == b"ANIM")
}

/// png / 静态 webp → JPEG。
///
/// JPEG 没有透明通道：先把图叠到白底上再编码。`to_rgb8()` 只是丢掉 alpha、
/// 不做合成，带透明区域的封面会变成黑块（白底至少是「纸」而不是「洞」）。
fn to_jpeg(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("封面解码失败: {e}"))?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err("封面尺寸为 0".into());
    }
    let mut canvas = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255]));
    image::imageops::overlay(&mut canvas, &img.to_rgba8(), 0, 0);
    let rgb = image::DynamicImage::ImageRgba8(canvas).to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
        .encode(rgb.as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("封面转码失败: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wpem-cover-test-{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn png_bytes() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(4, 2, image::Rgb([200, 30, 40]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn jpeg_bytes() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(4, 2, image::Rgb([10, 200, 40]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
            .encode(img.as_raw(), 4, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        out
    }

    /// 静态 webp（VP8L）
    fn static_webp_bytes() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(4, 2, image::Rgb([10, 40, 200]));
        let mut out = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut out)
            .encode(img.as_raw(), 4, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        out
    }

    /// 动图 webp 的最小容器外壳：只认 RIFF/WEBP/ANIM 块名，
    /// store_cover 对动图走原样存储、不解码，所以合成字节即可
    fn animated_webp_bytes() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&24u32.to_le_bytes());
        v.extend_from_slice(b"WEBP");
        v.extend_from_slice(b"VP8X");
        v.extend_from_slice(&10u32.to_le_bytes());
        v.extend_from_slice(&[0; 10]);
        v.extend_from_slice(b"ANIM");
        v
    }

    #[test]
    fn sniff_recognizes_magic_bytes_not_extensions() {
        assert_eq!(sniff(&jpeg_bytes()), Some(ImageKind::Jpeg));
        assert_eq!(sniff(&png_bytes()), Some(ImageKind::Png));
        assert_eq!(sniff(&static_webp_bytes()), Some(ImageKind::Webp));
        assert_eq!(sniff(b"GIF89a...."), Some(ImageKind::Gif));
        // 远端返回错误页/空响应：认不出来，绝不能当成图片写盘
        assert_eq!(sniff(b""), None);
        assert_eq!(sniff(b"<!DOCTYPE html><html>404</html>"), None);
    }

    #[test]
    fn store_cover_converts_png_to_jpg() {
        let dir = tmpdir("png2jpg");
        let out = store_cover(&dir, &png_bytes()).unwrap();
        assert_eq!(out.file_name().unwrap(), "preview.jpg");
        let written = std::fs::read(&out).unwrap();
        // 真的 JPEG（魔数 + 能被解回来），且尺寸不变
        assert_eq!(sniff(&written), Some(ImageKind::Jpeg));
        let back = image::load_from_memory(&written).unwrap();
        assert_eq!((back.width(), back.height()), (4, 2));
        // 不得留下对不上的 preview.png
        assert!(!dir.join("preview.png").exists());
        // 落盘临时文件要清干净（内容服务器会把目录暴露出去）
        assert!(!dir.join(PART_NAME).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_cover_keeps_gif_and_jpeg_verbatim() {
        let dir = tmpdir("verbatim");
        let gif = b"GIF89a-fake-but-magic-ok".to_vec();
        let out = store_cover(&dir, &gif).unwrap();
        assert_eq!(out.file_name().unwrap(), "preview.gif");
        assert_eq!(std::fs::read(&out).unwrap(), gif, "gif 必须原样落盘");

        let dir2 = tmpdir("verbatim-jpg");
        let jpg = jpeg_bytes();
        let out = store_cover(&dir2, &jpg).unwrap();
        assert_eq!(out.file_name().unwrap(), "preview.jpg");
        assert_eq!(
            std::fs::read(&out).unwrap(),
            jpg,
            "已是 jpeg 就不再编码一次（避免掉画质）"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn store_cover_converts_static_webp_but_keeps_animated_one() {
        let dir = tmpdir("webp");
        let out = store_cover(&dir, &static_webp_bytes()).unwrap();
        assert_eq!(out.file_name().unwrap(), "preview.jpg");
        assert!(image::load_from_memory(&std::fs::read(&out).unwrap()).is_ok());

        // 动图转 jpg 会丢动画：保留 webp（文件名与内容一致，仍然能当封面用）
        let dir2 = tmpdir("webp-anim");
        let anim = animated_webp_bytes();
        let out = store_cover(&dir2, &anim).unwrap();
        assert_eq!(out.file_name().unwrap(), "preview.webp");
        assert_eq!(std::fs::read(&out).unwrap(), anim);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn store_cover_rejects_non_image() {
        let dir = tmpdir("notimage");
        assert!(store_cover(&dir, b"<html>404</html>").is_err());
        assert!(existing_cover(&dir).is_none(), "失败不得留下任何 preview.*");
        assert!(!dir.join(PART_NAME).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_cover_finds_every_preview_ext_and_ignores_part_file() {
        let dir = tmpdir("existing");
        assert!(existing_cover(&dir).is_none());
        // 半截临时文件不算封面（否则补齐会被自己写了一半的文件挡住）
        std::fs::write(dir.join(PART_NAME), b"x").unwrap();
        assert!(existing_cover(&dir).is_none());
        // 任一 preview.<ext> 都算数，且优先级与下发给前端的清单一致
        std::fs::write(dir.join("preview.webp"), b"x").unwrap();
        assert_eq!(
            existing_cover(&dir).unwrap().file_name().unwrap(),
            "preview.webp"
        );
        std::fs::write(dir.join("preview.gif"), b"x").unwrap();
        assert_eq!(
            existing_cover(&dir).unwrap().file_name().unwrap(),
            "preview.gif"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------- 下载路径（本地 mock 服务，不碰真实 CDN） ----------

    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// 统计 mock 被请求的次数用
    fn hits(n: usize) -> (std::sync::Arc<std::sync::atomic::AtomicUsize>, impl Fn() -> usize) {
        let c = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(n));
        (c.clone(), move || c.load(Ordering::SeqCst))
    }

    /// 真实 CDN 的封面地址是**没有扩展名**的（…/ugc/<id>/<hash>/），
    /// 所以这里也故意用不带扩展名的路径 + 只给 Content-Type
    /// 真实 CDN 的封面地址是**没有扩展名**的（…/ugc/<id>/<hash>/），
    /// 所以这里也故意用不带扩展名的路径 + 只给 Content-Type
    #[tokio::test]
    async fn fetch_into_downloads_and_normalizes_to_local_file() {
        let png = png_bytes();
        let sent = png.clone();
        let (counter, hit) = hits(0);
        let app = axum::Router::new().route(
            "/ugc/1/abc/",
            axum::routing::get({
                let counter = counter.clone();
                move || {
                    let png = sent.clone();
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        (
                            [(axum::http::header::CONTENT_TYPE, "image/png")],
                            png,
                        )
                    }
                }
            }),
        );
        let base = serve(app).await;
        let dir = tmpdir("fetch");
        let client = reqwest::Client::new();

        let out = fetch_into(&client, &dir, &format!("{base}/ugc/1/abc/"), MAX_COVER_BYTES)
            .await
            .unwrap();
        assert_eq!(out.file_name().unwrap(), "preview.jpg");
        assert_eq!(sniff(&std::fs::read(&out).unwrap()), Some(ImageKind::Jpeg));
        assert_eq!(hit(), 1);

        // 第二次：目录里已有封面 → 直接用本地那份，不再发请求
        let again = fetch_into(&client, &dir, &format!("{base}/ugc/1/abc/"), MAX_COVER_BYTES)
            .await
            .unwrap();
        assert_eq!(again, out);
        assert_eq!(hit(), 1, "已有封面时不得再打网络");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fetch_into_keeps_existing_cover_and_never_overwrites() {
        let dir = tmpdir("no-overwrite");
        // 用户/作者自己放的封面（WE 工程自带的那份）
        let mine = b"GIF89a-author-owned".to_vec();
        std::fs::write(dir.join("preview.gif"), &mine).unwrap();

        let (counter, hit) = hits(0);
        let app = axum::Router::new().route(
            "/cover",
            axum::routing::get({
                let counter = counter.clone();
                move || {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        png_bytes()
                    }
                }
            }),
        );
        let base = serve(app).await;
        let client = reqwest::Client::new();

        let out = fetch_into(&client, &dir, &format!("{base}/cover"), MAX_COVER_BYTES)
            .await
            .unwrap();
        assert_eq!(out.file_name().unwrap(), "preview.gif");
        assert_eq!(hit(), 0, "已有封面就不该发请求");
        assert_eq!(std::fs::read(&out).unwrap(), mine, "作者封面一个字都不许改");
        assert!(!dir.join("preview.jpg").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fetch_into_rejects_error_status_and_bad_body() {
        let app = axum::Router::new()
            .route(
                "/404",
                axum::routing::get(|| async { (axum::http::StatusCode::NOT_FOUND, "nope") }),
            )
            // 200 但内容其实是错误页：也只能拒绝，不能在目录里留下假封面
            .route(
                "/html",
                axum::routing::get(|| async { ([(axum::http::header::CONTENT_TYPE, "text/html")], "<html>oops</html>") }),
            );
        let base = serve(app).await;
        let client = reqwest::Client::new();

        for path in ["/404", "/html"] {
            let dir = tmpdir(&format!("bad{}", path.trim_start_matches('/')));
            let err = fetch_into(&client, &dir, &format!("{base}{path}"), MAX_COVER_BYTES)
                .await
                .unwrap_err();
            assert!(!err.is_empty());
            assert!(existing_cover(&dir).is_none(), "{path}: 失败不得留下预览图");
            assert!(!dir.join(PART_NAME).exists(), "{path}: 临时文件要清掉");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[tokio::test]
    async fn fetch_into_aborts_when_body_exceeds_cap() {
        // cap 传小值：不是为了改生产上限，而是让「越界即中断」这条能被测到
        let app = axum::Router::new().route(
            "/big",
            axum::routing::get(|| async { vec![b'x'; 8192] }),
        );
        let base = serve(app).await;
        let client = reqwest::Client::new();
        let dir = tmpdir("cap");
        let err = fetch_into(&client, &dir, &format!("{base}/big"), 4096)
            .await
            .unwrap_err();
        assert!(err.contains("体积上限"), "应报体积上限，实际: {err}");
        assert!(existing_cover(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
