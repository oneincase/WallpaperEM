//! 通用工具函数

/// Windows 子进程创建标志：不新建控制台窗口。
///
/// 打包成 GUI 程序后 spawn 控制台程序（steamcmd.exe / ffmpeg.exe / tar.exe / cmd.exe），
/// 默认会**弹出一个控制台窗口**并一直挂到子进程结束 —— 下载壁纸时每调一次 steamcmd
/// 就闪一个黑窗。加上这个标志让子进程在无窗口模式下运行。
#[cfg(target_os = "windows")]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 让 `std::process::Command` 不弹控制台（非 Windows 为空操作）。
pub fn hide_console(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// 让 `tokio::process::Command` 不弹控制台（非 Windows 为空操作）。
pub fn hide_console_tokio(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(target_os = "windows")]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// URL query 值编码（最小实现，RFC 3986 unreserved 之外的字节百分号编码）
pub fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 生成随机 hex 字符串（用于会话 token / state）
pub fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// HTML 实体解码（对齐 Web 版 browse.ts decodeEntities）
pub fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}
