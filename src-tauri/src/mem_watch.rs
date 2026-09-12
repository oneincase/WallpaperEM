//! 内存观测：把「本进程 + 归属本应用的 WebKit 进程」的常驻占用打成一行日志。
//!
//! 为什么需要它：macOS 上壁纸页跑在 WebKit 的子进程里（WebContent / GPU /
//! Networking），说「内存漏了」时最要紧的是漏在**哪一个**进程 —— 我们自己的进程，
//! 还是某个 WebContent，以及它是在哪一次换壁纸 / 销毁窗口之后涨上去的。以前这只能
//! 靠活动监视器截图（不能 grep、也说不清时间线）。这里用 libproc 读每个进程的
//! `phys_footprint`（与活动监视器「内存」列同源），每 60s 一次、外加每次换壁纸与
//! 销毁窗口后各一次，和其它日志逐条对照即可定位。
//!
//! **归属判定**（第一版这里踩过坑）：macOS 上 WebKit 的这些进程是 launchd 通过 XPC
//! 起来的，`proc_listchildpids` **一个都看不到**（实测子进程列表里只有媒体适配器的
//! perl）。归属改用 libsystem 的 `responsibility_get_pid_responsible_for_pid` ——
//! WebKit 自己就是用这个把进程归到宿主 App 的：实测我们这 4 个 WebKit 进程都回我们的
//! pid，别的 App 的回它们自己的。它是 SPI，用 dlsym 取，取不到就只报本进程与直接
//! 子进程（并在日志里说明读数不完整）。
//!
//! 纯只读、无副作用；只在 macOS 上有实现，其它平台是空操作（日志里没有这一段）。

use std::sync::atomic::{AtomicBool, Ordering};

/// 一个进程的内存读数
#[derive(Debug, Clone)]
pub struct ProcMem {
    pub pid: i32,
    /// 进程名（WebKit 的是 `com.apple.WebKit.WebContent` 之类）
    pub name: String,
    /// `phys_footprint`（字节），与活动监视器「内存」列同源
    pub bytes: u64,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::ProcMem;
    use std::ffi::c_void;

    /// `dlsym(RTLD_DEFAULT, …)` 的句柄：Darwin 上 RTLD_DEFAULT 就是 -2
    ///（libc 0.2 没有导出这个常量）
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

    /// 读一次：第 0 个是本进程，其后是直接子进程 + 归属本进程的 WebKit 进程
    pub fn snapshot() -> Vec<ProcMem> {
        let me = std::process::id() as libc::pid_t;
        let mut out = Vec::new();
        if let Some(own) = read_pid(me) {
            out.push(own);
        }
        for pid in direct_children(me) {
            if let Some(m) = read_pid(pid) {
                out.push(m);
            }
        }
        if let Some(responsible_of) = responsible_of() {
            let known = out.len();
            for pid in all_pids() {
                if out[..known].iter().any(|p| p.pid == pid) {
                    continue;
                }
                if !proc_name_of(pid).starts_with("com.apple.WebKit") {
                    continue;
                }
                // SAFETY: 取到的是 libsystem 里同签名的函数（WebKit 的
                // ResponsibilitySPI.h 用的就是它），只读一个 pid 的归属
                if unsafe { responsible_of(pid) } != me {
                    continue;
                }
                if let Some(m) = read_pid(pid) {
                    out.push(m);
                }
            }
        }
        out
    }

    /// WebKit 进程归属能不能查到（查不到时读数只剩本进程 + 直接子进程）
    pub fn attribution_available() -> bool {
        responsible_of().is_some()
    }

    /// 直接子进程（perl 媒体适配器等）。注意 WebKit 不在这里，见文件头。
    fn direct_children(ppid: libc::pid_t) -> Vec<libc::pid_t> {
        const CAP: usize = 128;
        let mut pids = vec![0 as libc::pid_t; CAP];
        let rc = unsafe {
            libc::proc_listchildpids(
                ppid,
                pids.as_mut_ptr().cast(),
                (CAP * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
            )
        };
        if rc <= 0 {
            return Vec::new();
        }
        // 缓冲先填零、读到 0 为止：proc_listchildpids 的返回值在不同 SDK 上
        // 「写入的 pid 数」和「写入的字节数」两种说法都有，零结尾判据两种都吃
        pids.into_iter().take_while(|p| *p > 0).collect()
    }

    /// 系统里的全部 pid（同样按零结尾判据截断）
    pub fn all_pids() -> Vec<libc::pid_t> {
        let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if n <= 0 {
            return Vec::new();
        }
        let cap = (n as usize).saturating_add(64);
        let mut pids = vec![0 as libc::pid_t; cap];
        let got = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr().cast(),
                (cap * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
            )
        };
        if got <= 0 {
            return Vec::new();
        }
        pids.into_iter().take_while(|p| *p > 0).collect()
    }

    /// 单个进程的 footprint + 名字
    fn read_pid(pid: libc::pid_t) -> Option<ProcMem> {
        // SAFETY: rusage_info_v2 按 flavor 的布局零初始化后传入，长度匹配；
        // libproc 只往这块栈内存里写，不持有指针。
        let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid,
                libc::RUSAGE_INFO_V2,
                (&mut info as *mut libc::rusage_info_v2).cast::<libc::rusage_info_t>(),
            )
        };
        if rc != 0 {
            return None;
        }
        Some(ProcMem {
            pid,
            name: proc_name_of(pid),
            bytes: info.ri_phys_footprint,
        })
    }

    /// 进程名（取不到给 "?"，不影响读数）
    fn proc_name_of(pid: libc::pid_t) -> String {
        let mut buf = [0u8; 64];
        let rc = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        if rc <= 0 {
            return "?".to_string();
        }
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    }

    /// 这个 pid 现在是不是一个 WebKit 进程（进程名以 `com.apple.WebKit` 开头）
    pub fn is_webkit_process(pid: i32) -> bool {
        proc_name_of(pid).starts_with("com.apple.WebKit")
    }

    /// 取 `responsibility_get_pid_responsible_for_pid`（libsystem_coreservices 的 SPI）。
    ///
    /// 用 dlsym 取而不是直接 extern 声明：声明了但系统没有这个符号会在加载期直接炸，
    /// 取不到顶多少一段读数。
    fn responsible_of() -> Option<unsafe extern "C" fn(libc::pid_t) -> libc::pid_t> {
        use std::sync::OnceLock;
        type Fn = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;
        static FN: OnceLock<Option<Fn>> = OnceLock::new();
        *FN.get_or_init(|| {
            let sym = unsafe {
                libc::dlsym(
                    RTLD_DEFAULT,
                    c"responsibility_get_pid_responsible_for_pid".as_ptr(),
                )
            };
            if sym.is_null() {
                return None;
            }
            // SAFETY: 符号名与签名由 libsystem 提供，WebKit 的 ResponsibilitySPI.h
            // 用的是同一份声明（pid_t(pid_t)，C 调用约定）
            Some(unsafe { std::mem::transmute::<*mut c_void, Fn>(sym) })
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::ProcMem;

    pub fn snapshot() -> Vec<ProcMem> {
        Vec::new()
    }

    pub fn attribution_available() -> bool {
        false
    }

}

/// 归属进程合计超过这个值就打 WARN（正常空闲时约 200～400MB）
const WARN_TOTAL: u64 = 1500 * 1024 * 1024;
/// 单个进程超过这个值也打 WARN（正常 WebContent 约 40～200MB）
const WARN_SINGLE: u64 = 800 * 1024 * 1024;
/// 周期上报间隔
const PERIOD: std::time::Duration = std::time::Duration::from_secs(60);
/// 「归属查不到」只提醒一次
static WARNED_NO_ATTRIBUTION: AtomicBool = AtomicBool::new(false);

/// 起一个后台任务：每 60s 记一行内存观测（非 macOS 上什么都不做）
pub fn start_periodic_report() {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(PERIOD).await;
            report("周期");
        }
    });
}

/// 记一行内存观测。超过阈值记 WARN（便于直接 grep WARN 看有没有失控），
/// 否则记 INFO。`tag` 说明这次读数是在什么之后取的（换壁纸 / 销毁窗口 / 周期）。
pub fn report(tag: &str) {
    let all = imp::snapshot();
    let Some((own, others)) = all.split_first() else {
        return;
    };
    if !imp::attribution_available() && !WARNED_NO_ATTRIBUTION.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            "内存观测：拿不到 WebKit 进程归属（responsibility SPI 不可用），读数只含本进程与直接子进程"
        );
    }
    let total: u64 = others.iter().map(|p| p.bytes).sum();
    let line = summary_line(own, others);
    if total > WARN_TOTAL || others.iter().any(|p| p.bytes > WARN_SINGLE) {
        tracing::warn!("内存观测（{tag}）: {line}");
    } else {
        tracing::info!("内存观测（{tag}）: {line}");
    }
}

/// 当前全部 WebKit **WebContent** 进程的合计占用（字节）；非 macOS 返回 0。
///
/// 给「复用进程占用超预算就改走销毁重建」这条策略用（见 `wallpaper::schedule_window_reload`）——
/// 只要 WebContent，不算 GPU / Networking：那两份是全局共享、不随换壁纸累积，
/// 算进来会让预算一直被 GPU 的正常占用顶穿。
pub fn webcontent_footprint() -> u64 {
    imp::snapshot()
        .iter()
        .filter(|p| classify(&p.name) == "WebContent")
        .map(|p| p.bytes)
        .sum()
}

/// 这个 pid 现在是不是一个 WebKit 进程。
///
/// 给「按 pid 结束进程」当护栏用：pid 是会被系统回收复用的，取到 pid 到真正发信号
/// 之间隔了几毫秒（中间还要销毁窗口），那时它可能已经属于别的进程了 —— 再发 SIGKILL
/// 就是误杀。只有 macOS 那条路（[`crate::wallpaper::macos`]）用它。
#[cfg(target_os = "macos")]
pub fn is_webkit_process(pid: i32) -> bool {
    imp::is_webkit_process(pid)
}

/// 汇总成一行：`本进程 39MB；归属进程 4 个合计 1.45GB（WebContent2 1.41GB / GPU 22MB）`
/// 另外单列最大的那个（>512MB 时），1.39GB 那种残留一眼就能看见。
fn summary_line(own: &ProcMem, others: &[ProcMem]) -> String {
    let mut line = format!("本进程 {}", mb(own.bytes));
    if others.is_empty() {
        return format!("{line}；无归属进程");
    }
    let total: u64 = others.iter().map(|p| p.bytes).sum();
    line.push_str(&format!(
        "；归属进程 {} 个合计 {}",
        others.len(),
        mem(total)
    ));
    // 按类别归并（WebKit 的进程名：com.apple.WebKit.WebContent / .GPU / .Networking）
    for class in ["WebContent", "GPU", "Networking", "其它"] {
        let group: Vec<&ProcMem> = others
            .iter()
            .filter(|p| classify(&p.name) == class)
            .collect();
        if group.is_empty() {
            continue;
        }
        let bytes: u64 = group.iter().map(|p| p.bytes).sum();
        let count = group.len();
        line.push_str(&format!(" / {class}{count} {}", mem(bytes)));
    }
    if let Some(top) = others
        .iter()
        .max_by_key(|p| p.bytes)
        .filter(|p| p.bytes > 512 * 1024 * 1024)
    {
        line.push_str(&format!(
            "；最大 {} (pid {}, {})",
            mb(top.bytes),
            top.pid,
            classify(&top.name)
        ));
    }
    line
}

/// 进程名 → 类别
fn classify(name: &str) -> &'static str {
    if name.contains("WebContent") {
        "WebContent"
    } else if name.contains("GPU") {
        "GPU"
    } else if name.contains("Networking") {
        "Networking"
    } else {
        "其它"
    }
}

/// `12345678` → `12MB`
fn mb(bytes: u64) -> String {
    format!("{}MB", bytes / (1024 * 1024))
}

/// 总量：不足 1GB 用 MB，够 1GB 用 GB（保留两位，方便看趋势）
fn mem(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2}GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else {
        mb(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 读数自检：条目得自洽（自己排在第一个、数值为正、名字非空）。
    /// 沙箱/无权限时可能只拿到自己那条或一条都没有，所以不断言数量。
    #[test]
    fn snapshot_entries_are_plausible() {
        let all = imp::snapshot();
        let Some(own) = all.first() else {
            return;
        };
        assert_eq!(own.pid, std::process::id() as i32, "本进程应排在第一个");
        for p in &all {
            assert!(p.pid > 0);
            assert!(p.bytes > 0, "{} 的 footprint 应为正", p.name);
            assert!(!p.name.is_empty());
        }
    }

    /// 枚举系统进程这条链路要能跑通（本进程一定在列表里）
    #[cfg(target_os = "macos")]
    #[test]
    fn all_pids_contains_self() {
        let me = std::process::id() as i32;
        let pids = imp::all_pids();
        if pids.is_empty() {
            return; // 沙箱里可能被拒，不当作失败
        }
        assert!(pids.contains(&me), "枚举结果里应该有本进程");
    }

    /// 汇总行的分组与单位（不依赖真实读数）
    #[test]
    fn summary_groups_by_process_class() {
        let own = ProcMem {
            pid: 1,
            name: "WallpaperEM".into(),
            bytes: 200 * 1024 * 1024,
        };
        let kids = vec![
            ProcMem {
                pid: 2,
                name: "com.apple.WebKit.WebContent".into(),
                bytes: 1024 * 1024 * 1024,
            },
            ProcMem {
                pid: 3,
                name: "com.apple.WebKit.GPU".into(),
                bytes: 22 * 1024 * 1024,
            },
        ];
        let line = summary_line(&own, &kids);
        assert_eq!(
            line,
            "本进程 200MB；归属进程 2 个合计 1.02GB / WebContent1 1.00GB / GPU1 22MB；最大 1024MB (pid 2, WebContent)"
        );
    }

    /// 没有归属进程时不能出现空分组
    #[test]
    fn summary_without_others() {
        let own = ProcMem {
            pid: 1,
            name: "WallpaperEM".into(),
            bytes: 12 * 1024 * 1024,
        };
        assert_eq!(summary_line(&own, &[]), "本进程 12MB；无归属进程");
    }
}
