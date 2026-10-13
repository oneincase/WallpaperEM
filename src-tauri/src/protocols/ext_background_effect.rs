//! KWin 背景模糊协议对接（Wayland / Linux 专用，只依赖 wayland-* crates）。
//!
//! 协议 XML：`src/protocols/ext-background-effect-v1.xml`，逐字取自上游
//! wayland-protocols 的 `staging/ext-background-effect/ext-background-effect-v1.xml`
//!（2026-10-14 自 main 分支拉取；sha256
//! `e463b7863c97d7be05489b52b15cd0b3a5d8290b51340f62023b47d03b4217e3`；与 crates.io
//! wayland-protocols 0.32.13 内置的那一份逐字节相同）。接口版本 v1。
//!
//! 为什么自己生成绑定：该协议在 wayland-protocols crate 里被 `staging` feature 门控，
//! 打开它会把全部 staging 协议一起编进来；这里只生成这一个协议。wayland-scanner 0.31
//! 只提供 proc-macro（`generate_interfaces!` / `generate_client_code!`，0.30 时代的
//! `generate_code()` 构建脚本 API 已移除），所以在源码里展开；引用本文件的 blur.rs
//! 本身只在 `#[cfg(target_os = "linux")]` 下编译，生成代码自然只进 Linux 目标。
//!
//! 为什么两条路径都要走：KWin 侧两个协议互斥——Plasma 6.7+ 只提供
//! `ext_background_effect_manager_v1`（旧的 `org_kde_kwin_blur` 已被移除），
//! Plasma ≤6.6 只提供旧协议。故「先试新、bind 失败回退旧」，都不可用则返回 Err，
//! 由调用方（blur.rs）静默降级——本模块不注册任何 panic 源、也不重试。
//!
//! 协议版本参考（KWin 6.7 `src/wayland/ext_background_effect_v1.cpp`）：
//! `get_background_effect` 对已有效果对象的 surface 会抛 `background_effect_exists`
//! 协议错误（= 断开整条连接），调用方必须保证同一 surface 只调用一次。

#![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
#![allow(non_upper_case_globals, non_snake_case, unused_imports)]
#![allow(missing_docs, clippy::all)]

use std::error::Error;

use wayland_client::globals::{registry_queue_init, GlobalList, GlobalListContents};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur::OrgKdeKwinBlur;
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur_manager::OrgKdeKwinBlurManager;

use self::__interfaces::*;
use ext_background_effect_manager_v1::{
    Capability, Event as ManagerEvent, ExtBackgroundEffectManagerV1,
};
use ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1;

pub mod __interfaces {
    use wayland_client::protocol::__interfaces::*;
    wayland_scanner::generate_interfaces!("src/protocols/ext-background-effect-v1.xml");
}

// 生成代码按 `super::wayland_client::...` 取类型，故本模块必须把 wayland_client
// 作为名字引入（与 wayland-protocols crate 内部宏的写法一致）
use wayland_client;
use wayland_client::protocol::*;

wayland_scanner::generate_client_code!("src/protocols/ext-background-effect-v1.xml");

/// 整窗模糊区域边长：blur region 由合成器按 surface 尺寸裁剪，取一个比任何窗口
/// 都大的值即可；32767 同时避免 KWin 内部 QRegion 的整数运算溢出。
const BLUR_REGION_EXTENT: i32 = 32767;

/// 模糊最终落到的协议路径（日志要能区分三态：新协议 / 旧协议 / 都不可用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlurPath {
    /// Plasma 6.7+：`ext_background_effect_manager_v1`
    ExtBackgroundEffect,
    /// Plasma ≤6.6：`org_kde_kwin_blur`（新协议 bind 失败后的回退）
    PlasmaLegacy,
}

/// 一次模糊请求的结果（日志用；失败以 Err 返回，不在这里打日志）。
#[derive(Debug)]
pub struct Outcome {
    pub path: BlurPath,
    /// manager capabilities 事件：Some(true) 合成器声明支持 blur，Some(false) 未声明，
    /// None = 没收到事件（仅新协议路径有值）
    pub blur_capable: Option<bool>,
    /// 新协议失败并回退旧协议时的原因（成功走新协议时为 None）
    pub fallback_reason: Option<String>,
}

#[derive(Default)]
struct BlurState {
    blur_capable: Option<bool>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for BlurState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// manager 是唯一有事件的接口（capabilities），必须真处理；其余接口无事件，
/// 用 delegate_noop!（真收到事件说明协议实现有问题，unreachable! 会暴露它）。
impl Dispatch<ExtBackgroundEffectManagerV1, ()> for BlurState {
    fn event(
        state: &mut Self,
        _: &ExtBackgroundEffectManagerV1,
        event: ManagerEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // manager 只有这一个事件，用 let 而非 if let（后者会被 irrefutable_let_patterns 警告）
        let ManagerEvent::Capabilities { flags } = event;
        state.blur_capable =
            Some(matches!(flags, WEnum::Value(c) if c.contains(Capability::Blur)));
    }
}

delegate_noop!(BlurState: ExtBackgroundEffectSurfaceV1);
delegate_noop!(BlurState: WlCompositor);
delegate_noop!(BlurState: WlRegion);
delegate_noop!(BlurState: OrgKdeKwinBlurManager);
delegate_noop!(BlurState: OrgKdeKwinBlur);

/// 为 GDK 的 wl_surface 请求背景模糊：新协议优先，旧协议回退。
///
/// `surface` 必须是 borrowed foreign proxy（blur.rs 里由 GDK 指针包装而来）；
/// 只发送请求、不进入 GTK 主循环以外的任何事件循环。
pub fn request(conn: &Connection, surface: &WlSurface) -> Result<Outcome, Box<dyn Error>> {
    let (globals, mut queue) = registry_queue_init::<BlurState>(conn)?;
    let qh = queue.handle();
    let mut state = BlurState::default();

    match request_ext_background_effect(&globals, &qh, &mut queue, &mut state, surface) {
        Ok(()) => {
            conn.flush()?;
            Ok(Outcome {
                path: BlurPath::ExtBackgroundEffect,
                blur_capable: state.blur_capable,
                fallback_reason: None,
            })
        }
        Err(e) => {
            // 回退原因带上去（Plasma ≤6.6 上这是正常路径，不是错误）
            let reason = e.to_string();
            request_plasma_legacy(&globals, &qh, surface)?;
            conn.flush()?;
            Ok(Outcome {
                path: BlurPath::PlasmaLegacy,
                blur_capable: None,
                fallback_reason: Some(reason),
            })
        }
    }
}

/// Plasma 6.7+ 路径：bind v1 → `get_background_effect` → `set_blur_region`（整窗）
/// → `wl_surface.commit`（blur region 是 double-buffered 状态，不 commit 不生效）。
fn request_ext_background_effect(
    globals: &GlobalList,
    qh: &QueueHandle<BlurState>,
    queue: &mut EventQueue<BlurState>,
    state: &mut BlurState,
    surface: &WlSurface,
) -> Result<(), Box<dyn Error>> {
    // 版本范围必须落在接口定义内：wayland-client 对超出范围的 bind 会 panic 而非 Err，
    // XML 里两个接口都是 v1，所以精确请求 1..=1（旧协议那条同样只到 v1）
    let manager: ExtBackgroundEffectManagerV1 = globals.bind(qh, 1..=1, ())?;
    // capabilities 事件在 bind 后立刻下发，roundtrip 一次把它取回来（只看日志用，
    // 不参与成败判定：没有 blur 能力时仍设置 region，用户后续开启模糊效果即生效）
    queue.roundtrip(state)?;

    // blur region 初始为空（= 不模糊），必须显式设置；参数是 wl_region，NULL 表示移除
    // 效果，所以给一个远大于任何窗口的矩形，由合成器裁剪到 surface 尺寸。
    // 注：从 registry 再 bind 一个 wl_compositor 只为造 region（wl_compositor 不是
    // XML 里 singleton 的 global，服务端允许多个 bind）；复用 GDK 的那份需要额外的
    // GDK 私有符号，不值得。region 有拷贝语义，用完即弃。
    let compositor: WlCompositor = globals.bind(qh, 1..=1, ())?;
    let effect: ExtBackgroundEffectSurfaceV1 = manager.get_background_effect(surface, qh, ());
    let region: WlRegion = compositor.create_region(qh, ());
    region.add(0, 0, BLUR_REGION_EXTENT, BLUR_REGION_EXTENT);
    effect.set_blur_region(Some(&region));

    // 显式 commit：新协议没有自己的 commit 请求（旧协议有），直接复用 wl_surface.commit。
    // 只提交待提交状态、不挂新 buffer；GTK 自己的下一次 commit 同样是合法的。
    surface.commit();
    Ok(())
}

/// Plasma ≤6.6 路径：`org_kde_kwin_blur_manager.create(surface)` + 该协议自己的 `commit`。
/// 接口无事件，proxy 丢弃后合成器侧 blur 随 surface 生命周期保留。
fn request_plasma_legacy(
    globals: &GlobalList,
    qh: &QueueHandle<BlurState>,
    surface: &WlSurface,
) -> Result<(), Box<dyn Error>> {
    let manager: OrgKdeKwinBlurManager = globals.bind(qh, 1..=1, ())?;
    let blur = manager.create(surface, qh, ());
    blur.commit();
    Ok(())
}
