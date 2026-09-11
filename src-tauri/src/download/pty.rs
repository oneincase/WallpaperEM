//! 子进程交互通道：给需要交互输入的子进程（steamcmd）分配终端 / 管道。
//!
//! 平台差异（对外接口一致：`Pty::open` / `attach` / `channel`）：
//!
//! - **macOS / Linux**：POSIX 伪终端（`posix_openpt/grantpt/unlockpt/ptsname`）。
//!   steamcmd 的 *nix 构建检测到 stdin 不是终端时会直接打印
//!   "steamcmd run with --non-interactive on its command line; cannot read from
//!   the console" 然后退出，因此密码/验证码交互必须走真 PTY 而不是管道。
//!   master 端留在父进程包成 `tokio::fs::File` 做异步读写；slave 端通过
//!   `pre_exec` 设为子进程的 stdin/stdout/stderr，并 `setsid` + `TIOCSCTTY`
//!   让子进程真正拥有控制终端（否则某些终端检测仍会失败）。
//!   PTY 下 stdout/stderr 合流到同一个 master fd，这是终端语义决定的，调用方只需读一路。
//!
//! - **Windows**：steamcmd.exe 没有 *nix 那套 TTY 判定 —— stdin 是匿名管道时
//!   `ReadFile` 照样能读（管道与控制台输入在 Win32 层是同一套 API），所以用三根
//!   匿名管道（stdin 写、stdout/stderr 读）即可，不必自建 ConPTY（ConPTY 必须用
//!   带 `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` 的 `CreateProcessW` 启动子进程，
//!   与 `tokio::process::Command` 不兼容）。stderr 由独立任务转发进同一条事件流，
//!   调用方同样只读一路。

use std::io;

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

/// PTY 在子进程退出时返回 EIO，这是正常终止信号而非故障（Windows 管道走 EOF）
#[cfg(unix)]
fn is_eof_error(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::EIO)
}
#[cfg(not(unix))]
fn is_eof_error(_e: &io::Error) -> bool {
    false
}

/// 字节级读取子进程输出：拆出完整行，同时用 `is_prompt` 探测未换行的提示。
///
/// `is_prompt` 收到的是「尚未成行的尾部文本」，返回 true 表示这是一个待输入提示。
/// 同一段尾部文本只会上报一次，避免重复触发。
// Windows 的 channel 走 spawn_byte_reader_into（要与 stderr 共用发送端），
// 这个便利包装只有 *nix 与单元测试用得到
#[cfg(any(unix, test))]
pub fn spawn_byte_reader<F>(
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    is_prompt: F,
) -> tokio::sync::mpsc::Receiver<OutEvent>
where
    F: Fn(&str) -> bool + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel::<OutEvent>(256);
    spawn_byte_reader_into(reader, is_prompt, tx);
    rx
}

/// 同 [`spawn_byte_reader`]，但复用调用方给的发送端 —— Windows 上 stdout 与
/// stderr 两路要汇进同一条事件流，只有共用 tx 才能做到。
pub(crate) fn spawn_byte_reader_into<F>(
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    is_prompt: F,
    tx: tokio::sync::mpsc::Sender<OutEvent>,
) where
    F: Fn(&str) -> bool + Send + 'static,
{
    use tokio::io::AsyncReadExt;

    tauri::async_runtime::spawn(async move {
        let mut reader = reader;
        let mut pending: Vec<u8> = Vec::new();
        let mut buf = [0u8; 4096];
        let mut last_prompt = String::new();
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Err(e) => {
                    if !is_eof_error(&e) {
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
                    // steamcmd 自更新阶段用 \r 原地刷新进度（不带 \n）：也在这里拆一次，
                    // 否则整段进度会攒成一个巨大的「单行」，前端只在结束时看到一次
                    if let Some(pos) = pending.iter().position(|&b| b == b'\r') {
                        let mut line: Vec<u8> = pending.drain(..=pos).collect();
                        line.pop();
                        let text = String::from_utf8_lossy(&line).to_string();
                        if !text.trim().is_empty() && tx.send(OutEvent::Line(text)).await.is_err() {
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
}

/// Windows 专用：把子进程 stderr 转发进同一条事件流（steamcmd 的错误/重试
/// 提示部分写在 stderr 上，丢掉会让失败原因只剩一句「下载失败」）。
#[cfg(windows)]
pub(crate) fn spawn_stderr_forwarder(
    stderr: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    tx: tokio::sync::mpsc::Sender<OutEvent>,
) {
    use tokio::io::AsyncReadExt;

    tauri::async_runtime::spawn(async move {
        let mut reader = stderr;
        let mut pending: Vec<u8> = Vec::new();
        let mut buf = [0u8; 2048];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Err(_) => break,
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
                        let mut line: Vec<u8> = pending.drain(..=pos).collect();
                        line.pop();
                        if line.last() == Some(&b'\r') {
                            line.pop();
                        }
                        let text = String::from_utf8_lossy(&line).to_string();
                        if text.trim().is_empty() {
                            continue;
                        }
                        tracing::debug!("steamcmd stderr: {text}");
                        if tx.send(OutEvent::Line(text)).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
        if !pending.is_empty() {
            let text = String::from_utf8_lossy(&pending).to_string();
            if !text.trim().is_empty() {
                let _ = tx.send(OutEvent::Line(text)).await;
            }
        }
    });
}

// ---------- 平台实现 ----------

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::io::{FromRawFd, RawFd};
    use std::path::PathBuf;

    use tokio::process::{Child, Command};

    use super::OutEvent;

    /// 子进程 stdin 的写出端（PTY master 是双工的，同一句柄既读又写）
    pub type Writer = tokio::fs::File;
    /// 子进程输出（PTY master 合流了 stdout/stderr）
    type Reader = tokio::fs::File;

    /// 已分配的 PTY：master 端读写句柄 + slave 端设备路径（仅用于日志）。
    pub struct Pty {
        /// master fd。所有权在此结构体，`take_master` 交出后置 -1。
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

        /// 交出 master 端为异步文件句柄（此后 `Pty` 不再拥有该 fd）
        fn take_master(&mut self) -> io::Result<tokio::fs::File> {
            let fd = std::mem::replace(&mut self.master, -1);
            if fd < 0 {
                return Err(io::Error::new(io::ErrorKind::Other, "PTY master 已交出"));
            }
            // SAFETY: fd 由 posix_openpt 获得且此处唯一所有，转交给 File 管理生命周期
            let std_file = unsafe { std::fs::File::from_raw_fd(fd) };
            Ok(tokio::fs::File::from_std(std_file))
        }

        /// 子进程 spawn 之后取交互通道（写出端 + 输出事件流）。
        ///
        /// master 是双工的：读子进程输出、写用户输入，所以 `try_clone` 出第二个
        /// 句柄专供读取。
        pub async fn channel(
            &mut self,
            _child: &mut Child,
            is_prompt: fn(&str) -> bool,
        ) -> io::Result<(Writer, tokio::sync::mpsc::Receiver<OutEvent>)> {
            let master = self.take_master()?;
            let reader: Reader = master.try_clone().await?;
            let rx = super::spawn_byte_reader(reader, is_prompt);
            Ok((master, rx))
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            // SAFETY: master 由本结构体独占；take_master 交出后为 -1，该分支不会走到
            if self.master >= 0 {
                unsafe {
                    libc::close(self.master);
                }
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::process::Stdio;

    use tokio::process::{Child, ChildStdin, Command};

    use super::OutEvent;

    /// 子进程 stdin 的写出端（tokio 管道句柄）
    pub type Writer = ChildStdin;
    /// 子进程输出（stdout，stderr 经转发任务汇入同一事件流）
    type Reader = tokio::process::ChildStdout;

    /// Windows 不需要预分配终端资源：管道在 spawn 时由 std 建立，
    /// 这里只保留一个占位结构以对齐平台层签名。
    pub struct Pty;

    impl Pty {
        pub fn open() -> io::Result<Self> {
            Ok(Pty)
        }

        /// 让子进程的 stdin/stdout/stderr 都走匿名管道。
        pub fn attach(&self, cmd: &mut Command) {
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        }

        /// 子进程 spawn 之后取交互通道：写出端 +（stdout ∪ stderr）事件流。
        pub async fn channel(
            &mut self,
            child: &mut Child,
            is_prompt: fn(&str) -> bool,
        ) -> io::Result<(Writer, tokio::sync::mpsc::Receiver<OutEvent>)> {
            let writer = child
                .stdin
                .take()
                .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "子进程 stdin 管道不可用"))?;
            let stdout: Reader = child
                .stdout
                .take()
                .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "子进程 stdout 管道不可用"))?;
            let stderr = child
                .stderr
                .take()
                .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "子进程 stderr 管道不可用"))?;

            let (tx, rx) = tokio::sync::mpsc::channel::<OutEvent>(256);
            super::spawn_byte_reader_into(stdout, is_prompt, tx.clone());
            super::spawn_stderr_forwarder(stderr, tx);
            Ok((writer, rx))
        }
    }
}

pub use imp::Pty;

#[cfg(test)]
mod tests {
    use super::*;

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

    #[cfg(unix)]
    #[test]
    fn open_allocates_a_pty() {
        let pty = Pty::open().expect("应能分配 PTY");
        let p = pty.slave_path.to_string_lossy().to_string();
        assert!(p.starts_with("/dev/"), "slave 路径异常: {p}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_sees_a_tty_through_pty() {
        use tokio::process::Command;

        let mut pty = Pty::open().expect("应能分配 PTY");
        let mut cmd = Command::new("/bin/sh");
        // 子进程自检 stdin 是否为终端：这正是 steamcmd 的判定条件
        cmd.arg("-c")
            .arg("test -t 0 && echo IS_TTY || echo NOT_TTY");
        pty.attach(&mut cmd);
        let mut child = cmd.spawn().expect("应能启动子进程");
        // 写出端必须活到子进程结束：它同时就是 PTY master
        let (_writer, mut rx) = pty
            .channel(&mut child, |_| false)
            .await
            .expect("应能取到交互通道");

        // 加超时兜底：读取若不能在子进程退出后终止（例如 master 被误设为非阻塞，
        // 导致 tokio::fs::File 收到 EAGAIN 却不注册 waker），这里必须失败而不是挂死。
        let collect = async {
            let mut lines = Vec::new();
            while let Some(ev) = rx.recv().await {
                if let OutEvent::Line(l) = ev {
                    lines.push(l);
                }
            }
            lines
        };
        let lines = tokio::time::timeout(std::time::Duration::from_secs(10), collect)
            .await
            .expect("PTY 读取未在子进程退出后终止（master 不应为非阻塞）");
        let _ = child.wait().await;
        let text = lines.join("\n");
        assert!(
            text.contains("IS_TTY"),
            "子进程未识别到 TTY，实际输出: {text:?}"
        );
    }
}
