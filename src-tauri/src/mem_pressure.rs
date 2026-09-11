//! 系统内存压力探测：给「主窗口只在内存压力下回收」用（见 [`crate::main_window`]）。
//!
//! 为什么需要它：主窗口（React UI）隐藏后曾经按固定时长闲置就销毁，指望 WebKit
//! 连带回收它的 WebContent 进程；实测 macOS 上那一步**回收不了内存**（默认共享
//! 存储没有按标识删除的 API，`destroy()` 只是把 WKWebView 摘下来，进程连页面一起
//! 留在 WebKit 的进程池里），只白付一次页面重载 + 隐藏瞬间的主线程停顿。改成
//! 「只在系统真的紧张时才回收」之后，这个探测就是唯一的触发信号。
//!
//! 三平台各用一条最省事的现成信号；**读不到就返回 `None`**，调用方按「无压力」
//! 处理 —— 宁可让主窗口常驻，也不要在没压力时白付一次窗口重建。

/// 一次读数：是否处于压力下 + 原始读数（写日志、事后核阈值用）
#[derive(Debug, Clone)]
pub struct Reading {
    /// 是否判定为「系统内存压力」
    pub pressure: bool,
    /// 原始读数文本，例如 `kern.memorystatus_vm_pressure_level=2（WARN）`
    pub detail: String,
}

/// 读一次系统内存压力；`None` = 本平台没有可用信号 / 本次读取失败
pub fn read() -> Option<Reading> {
    imp::read()
}

/// macOS：`kern.memorystatus_vm_pressure_level`
///
/// 取值与 GCD 的 memory-pressure source 同源（1 = NORMAL、2 = WARN、
/// 4 = CRITICAL），也就是系统真正在向应用广播的那个压力等级。
#[cfg(target_os = "macos")]
mod imp {
    use super::Reading;

    pub fn read() -> Option<Reading> {
        let mut level: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::size_t;
        // SAFETY: name 是静态 C 字符串；输出缓冲与 oldlen 都在本栈帧上且长度匹配；
        // newp/newlen 传空指针 = 只读查询。
        let rc = unsafe {
            libc::sysctlbyname(
                c"kern.memorystatus_vm_pressure_level".as_ptr(),
                (&mut level as *mut libc::c_int).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        let name = match level {
            1 => "NORMAL",
            2 => "WARN",
            4 => "CRITICAL",
            _ => "UNKNOWN",
        };
        Some(Reading {
            pressure: level >= 2,
            detail: format!("kern.memorystatus_vm_pressure_level={level}（{name}）"),
        })
    }
}

/// Windows：`GlobalMemoryStatusEx().dwMemoryLoad`
///
/// 物理内存占用百分比。用 90% 作为「有压力」的门槛 —— 与 macOS 那条的宽容度对齐
/// （WARN 才动手，不是一有占用就动手）。
#[cfg(target_os = "windows")]
mod imp {
    use super::Reading;
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    /// 物理内存占用率达到这个百分比即视为有压力
    const PRESSURE_LOAD_PERCENT: u32 = 90;

    pub fn read() -> Option<Reading> {
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // SAFETY: dwLength 已按 API 要求填成本结构体大小；失败直接返回 None。
        unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
        Some(Reading {
            pressure: status.dwMemoryLoad >= PRESSURE_LOAD_PERCENT,
            detail: format!("GlobalMemoryStatusEx dwMemoryLoad={}%", status.dwMemoryLoad),
        })
    }
}

/// Linux：`/proc/meminfo` 的 `MemAvailable / MemTotal`
///
/// 有 PSI（`/proc/pressure/memory`，内核 4.20+）会更准，但 MemAvailable 到处都是、
/// 也不用解析两种格式；主窗口回收只需要一个「大概紧不紧」的信号。
#[cfg(target_os = "linux")]
mod imp {
    use super::Reading;

    /// 可用内存低于总量的这个百分比即视为有压力
    const PRESSURE_AVAILABLE_PERCENT: u64 = 8;

    pub fn read() -> Option<Reading> {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let (mut total_kb, mut avail_kb) = (0u64, 0u64);
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                total_kb = parse_kb(rest)?;
            } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
                avail_kb = parse_kb(rest)?;
            }
        }
        if total_kb == 0 {
            return None;
        }
        let percent = avail_kb.saturating_mul(100) / total_kb;
        Some(Reading {
            pressure: percent < PRESSURE_AVAILABLE_PERCENT,
            detail: format!("MemAvailable={percent}%（{avail_kb} / {total_kb} KB）"),
        })
    }

    /// `MemTotal:  32749552 kB` → `32749552`
    fn parse_kb(s: &str) -> Option<u64> {
        s.split_whitespace().next()?.parse().ok()
    }
}

/// 其它平台：没有探测手段 → 返回 `None`（＝不回收，主窗口保持常驻）
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod imp {
    use super::Reading;

    pub fn read() -> Option<Reading> {
        None
    }
}
