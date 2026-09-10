//! PTY 支撑：给需要真 TTY 的子进程（steamcmd）分配伪终端。
//!
//! steamcmd 检测到 stdin 不是终端时会直接打印
//! "steamcmd run with --non-interactive on its command line; cannot read from the console"
//! 然后退出，因此密码/验证码交互必须走 PTY 而不是管道。
//!
//! 这里用 POSIX 的 `posix_openpt/grantpt/unlockpt/ptsname` 开 PTY：
//! - master 端留在父进程，包成 `tokio::fs::File` 做异步读写
//! - slave 端通过 `pre_exec` 设为子进程的 stdin/stdout/stderr，并 `setsid` + `TIOCSCTTY`
//!   让子进程真正拥有控制终端（否则某些终端检测仍会失败）
//!
//! PTY 下 stdout/stderr 会合流到同一个 master fd，这是终端语义决定的，调用方只需读一路。

use std::io;
use std::os::unix::io::{FromRawFd, RawFd};
use std::path::PathBuf;

use tokio::process::Command;

/// 已分配的 PTY：master 端读写句柄 + slave 端设备路径（仅用于日志）。
pub struct Pty {
    /// master fd。所有权在此结构体，Drop 时关闭。
    master: RawFd,
    #[allow(dead_code)]
    pub slave_path: PathBuf,
}

impl Pty {
    /// 分配一个新的 PTY 对。
    pub fn open() -> io::Result<Self> {
        // SAFETY: 全部是标准 POSIX PTY 调用，返回值逐个校验；
        // 失败路径会关闭已获得的 fd，不泄漏。
        unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            if master < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::grantpt(master) != 0 || libc::unlockpt(master) != 0 {
                let e = io::Error::last_os_error();
                libc::close(master);
                return Err(e);
            }
            let name = libc::ptsname(master);
            if name.is_null() {
                let e = io::Error::last_os_error();
                libc::close(master);
                return Err(e);
            }
            let slave_path = PathBuf::from(
                std::ffi::CStr::from_ptr(name)
                    .to_str()
                    .unwrap_or_default()
                    .to_string(),
            );
            // 刻意保持 master 为阻塞模式：读取侧用 tokio::fs::File（spawn_blocking 语义），
            // 若置 O_NONBLOCK 则 read 返回 EAGAIN，而 tokio::fs::File 不会为此注册 waker，
            // runtime 会永久 park。阻塞读在子进程退出（slave 全部关闭）时返回 EIO，
            // 这正是我们需要的终止信号。
            Ok(Pty { master, slave_path })
        }
    }

    /// 把 slave 端接到子进程的 stdin/stdout/stderr 上。
    ///
    /// slave fd 在 `pre_exec`（fork 之后、exec 之前）现开，避免父进程长期持有
    /// slave 端导致子进程退出后 master 读不到 EOF。
    pub fn attach(&self, cmd: &mut Command) {
        let slave_path = self.slave_path.clone();
        let master = self.master;
        // SAFETY: pre_exec 运行在 fork 之后的子进程里，只调用 async-signal-safe 的
        // open/dup2/close/setsid/ioctl，不分配内存、不加锁。
        unsafe {
            cmd.pre_exec(move || {
                // 新会话：子进程脱离父进程的控制终端，才能把 PTY 设为自己的控制终端
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                let c_path = std::ffi::CString::new(slave_path.as_os_str().as_encoded_bytes())
                    .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
                let slave = libc::open(c_path.as_ptr(), libc::O_RDWR);
                if slave < 0 {
                    return Err(io::Error::last_os_error());
                }
                // 设为控制终端（macOS 上 setsid 后需要显式 TIOCSCTTY）
                libc::ioctl(slave, libc::TIOCSCTTY as libc::c_ulong, 0);
                for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
                    if libc::dup2(slave, fd) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                if slave > libc::STDERR_FILENO {
                    libc::close(slave);
                }
                // 子进程不应继承 master 端，否则子进程退出后 master 仍有写者，读不到 EOF
                libc::close(master);
                Ok(())
            });
        }
    }

    /// 取出 master 端为异步文件句柄。调用后 `Pty` 不再拥有该 fd。
    pub fn into_async_file(self) -> io::Result<tokio::fs::File> {
        let fd = self.master;
        std::mem::forget(self); // 所有权移交给 File，避免 Drop 重复 close
                                // SAFETY: fd 由 posix_openpt 获得且此处唯一所有，转交给 File 管理生命周期
        let std_file = unsafe { std::fs::File::from_raw_fd(fd) };
        Ok(tokio::fs::File::from_std(std_file))
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // SAFETY: master 由本结构体独占；into_async_file 走了 mem::forget 不会到这里
        unsafe {
            libc::close(self.master);
        }
    }
}

/// 子进程输出事件：完整行，或「检测到交互提示」。
///
/// 交互提示（`password:` / `Steam Guard code:`）不带换行，
/// 按行读会永远等不到换行而漏检，因此必须按字节边读边匹配。
#[derive(Debug)]
pub enum OutEvent {
    Line(String),
    /// 检测到提示，附带原始待处理文本供调用方分类
    Prompt(String),
}

/// 字节级读取子进程输出：拆出完整行，同时用 `is_prompt` 探测未换行的提示。
///
/// `is_prompt` 收到的是「尚未成行的尾部文本」，返回 true 表示这是一个待输入提示。
/// 同一段尾部文本只会上报一次，避免重复触发。
pub fn spawn_byte_reader<F>(
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    is_prompt: F,
) -> tokio::sync::mpsc::Receiver<OutEvent>
where
    F: Fn(&str) -> bool + Send + 'static,
{
    use tokio::io::AsyncReadExt;

    let (tx, rx) = tokio::sync::mpsc::channel::<OutEvent>(256);
    tauri::async_runtime::spawn(async move {
        let mut reader = reader;
        let mut pending: Vec<u8> = Vec::new();
        let mut buf = [0u8; 4096];
        let mut last_prompt = String::new();
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Err(e) => {
                    // PTY 在子进程退出时会返回 EIO，这是正常终止信号而非故障
                    if e.raw_os_error() != Some(libc::EIO) {
                        tracing::debug!("pty reader error: {e}");
                    }
                    break;
                }
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    // 拆出完整行
                    while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
                        let mut line: Vec<u8> = pending.drain(..=pos).collect();
                        line.pop();
                        if line.last() == Some(&b'\r') {
                            line.pop(); // PTY 会把 \n 转成 \r\n
                        }
                        let text = String::from_utf8_lossy(&line).to_string();
                        if tx.send(OutEvent::Line(text)).await.is_err() {
                            return;
                        }
                        last_prompt.clear();
                    }
                    // 残留的尾部文本：可能是不带换行的输入提示
                    if !pending.is_empty() {
                        let tail = String::from_utf8_lossy(&pending).to_string();
                        if tail != last_prompt && is_prompt(&tail) {
                            last_prompt = tail.clone();
                            if tx.send(OutEvent::Prompt(tail)).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        }
        if !pending.is_empty() {
            let text = String::from_utf8_lossy(&pending).to_string();
            let _ = tx.send(OutEvent::Line(text)).await;
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[test]
    fn open_allocates_a_pty() {
        let pty = Pty::open().expect("应能分配 PTY");
        let p = pty.slave_path.to_string_lossy().to_string();
        assert!(p.starts_with("/dev/"), "slave 路径异常: {p}");
    }

    #[tokio::test]
    async fn child_sees_a_tty_through_pty() {
        let pty = Pty::open().expect("应能分配 PTY");
        let mut cmd = Command::new("/bin/sh");
        // 子进程自检 stdin 是否为终端：这正是 steamcmd 的判定条件
        cmd.arg("-c")
            .arg("test -t 0 && echo IS_TTY || echo NOT_TTY");
        pty.attach(&mut cmd);
        let mut child = cmd.spawn().expect("应能启动子进程");
        let mut master = pty.into_async_file().expect("master 转异步句柄");

        // 加超时兜底：读取若不能在子进程退出后终止（例如 master 被误设为非阻塞，
        // 导致 tokio::fs::File 收到 EAGAIN 却不注册 waker），这里必须失败而不是挂死。
        let collect = async {
            let mut out = Vec::new();
            let mut buf = [0u8; 1024];
            // 子进程退出后 PTY 读返回 EIO，以此作为结束条件
            loop {
                match master.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => out.extend_from_slice(&buf[..n]),
                    Err(_) => break,
                }
            }
            out
        };
        let out = tokio::time::timeout(std::time::Duration::from_secs(10), collect)
            .await
            .expect("PTY 读取未在子进程退出后终止（master 不应为非阻塞）");
        let _ = child.wait().await;
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("IS_TTY"),
            "子进程未识别到 TTY，实际输出: {text:?}"
        );
    }

    #[tokio::test]
    async fn byte_reader_splits_lines_and_detects_prompt() {
        // 构造一段「两行 + 末尾无换行提示」的输入
        let data = b"Loading Steam API...OK\r\nConnecting anonymously...\r\npassword:".to_vec();
        let cursor = std::io::Cursor::new(data);
        let mut rx = spawn_byte_reader(cursor, |tail| tail.to_lowercase().contains("password:"));

        let mut lines = Vec::new();
        let mut prompts = Vec::new();
        while let Some(ev) = rx.recv().await {
            match ev {
                OutEvent::Line(l) => lines.push(l),
                OutEvent::Prompt(p) => prompts.push(p),
            }
        }
        assert_eq!(lines[0], "Loading Steam API...OK");
        assert_eq!(lines[1], "Connecting anonymously...");
        assert_eq!(prompts, vec!["password:".to_string()]);
        // 末尾残留文本在 EOF 时也会作为一行补发
        assert!(lines.contains(&"password:".to_string()));
    }
}
