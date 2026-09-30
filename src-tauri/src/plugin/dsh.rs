//! 内置插件：DeepSeek Harness（dsh）。
//!
//! 这个插件做的只有一件事：让「打开 dsh」在本机**可重复地成功**。四步：
//!
//! 1. **扫描**：本机有没有 dsh 环境 —— PATH/常见全局目录里的 `dsh` CLI、桌面版
//!    （只作引导提示）、`$DSH_HOME`；顺手探测版本。
//! 2. **准备 profile**：在 `$DSH_HOME/profiles/wallpallperem` 放一个**干净
//!    profile** —— bundle 清单只有 dsh 自带的 `dsh-base` + `dsh-web-app`，没有
//!    任何第三方插件；文件写法与 dsh 自己的 `initProfile()` 完全一致（已存在的
//!    文件一律不动）。
//! 3. **嵌入 MCP**：把本应用的 MCP 服务（`http://127.0.0.1:<port>/mcp?token=…`）
//!    写成该 profile 的 patch 层条目 `wallpaperem-mcp`，于是 harness 里的模型
//!    多出 `mcp__wallpaperem__*` 一整套壁纸工具。
//! 4. **启动**：`dsh --profile wallpallperem --no-open --port <空闲端口>`，从它
//!    打印的 `dsh web: <url>` 行拿到**带进程令牌**的地址，再开一个应用窗口加载。
//!
//! 为什么不复用用户自己的 `web` profile：那份 profile 里是用户装的插件与模型
//! 配置，塞进本应用的 MCP 会污染它，「打开插件」的结果也会随用户改动漂移。干净
//! profile 换来的是每次打开都一样。
//!
//! 与 profile 的关系是「**托管但不重置**」：manifest 与 MCP 层由本应用维护，用户
//! 若已经把 `wallpallperem` 改成自己的样子（bundle 清单不同 / 手改过 patch），
//! 我们只往一个**独立 overlay 文件**里写 MCP 层并用 `--patch` 挂载，绝不覆盖用户
//! 内容。

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use super::window::open_url_window;
use super::PluginError;

/// 插件使用的 dsh profile 名。刻意用**非内置名**：web/headless/sdk/acp/desktop
/// 要么由 dsh 自己管理、要么被保留，只有自定义名才是「本应用的干净 profile」。
pub const PROFILE: &str = "wallpallperem";
/// 本应用 MCP 在 harness 侧注册的 serverName（工具名形如 `mcp__wallpaperem__<tool>`）
const MCP_SERVER: &str = "wallpaperem";
/// profile 的用户 patch 层文件名（dsh 约定的名字）
const PATCH_FILE: &str = "cordis.patch.yml";
/// 用户已自定义 patch 层时，MCP 层改放这个独立覆盖文件（--patch 挂载）
const OVERLAY_FILE: &str = "wallpaperem-mcp.patch.yml";
/// 托管标记：patch/overlay 文件里有它 = 这份是本应用写的，可以安全重写
const PATCH_MARKER: &str = "# managed-by: wallpaperem-plugin";
const PLUGIN_WINDOW_LABEL: &str = "plugin-dsh";
const WINDOW_TITLE: &str = "DeepSeek Harness";
/// 未安装 dsh 时的引导目标：官网下载页 + npm 全局安装命令
const DOWNLOAD_URL: &str = "https://harness.deepseek.com";
const INSTALL_COMMAND: &str = "npm install -g @deepseek-ai/dsh";
/// 首次启动要建 profile、挂载插件树，90s 是给冷启动留的余量
const START_TIMEOUT: Duration = Duration::from_secs(90);
/// `dsh --version` 探测超时：超了就当拿不到版本号，但进程必须先收掉
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// SIGTERM 到 SIGKILL 之间给多久。node 侧的收尾（放掉 profile 写锁、带走子进程）
/// 通常是毫秒级，半秒足够；等不到就上手 SIGKILL
const KILL_GRACE: Duration = Duration::from_millis(500);
/// 往 profile 装插件包（pnpm + 联网）：dsh 自己就会等最多 2 分钟的写锁/查找，
/// 这里给到 5 分钟
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
/// 诊断输出只留最后这么多行
const MAX_TAIL_LINES: usize = 40;
/// 全局装一个 pnpm（npm + 联网）：给 3 分钟够了
const PNPM_INSTALL_TIMEOUT: Duration = Duration::from_secs(180);

// ---------------------------------------------------------------- 错误

// ---------------------------------------------------------------- 状态

/// 插件宿主状态：dsh 子进程在**三种状态**之间走一圈。
///
/// 收尾（关窗 / 停止 / 退出）必须能同时收掉「已就绪」与「启动中」两个阶段 —— 冷启动
/// 要建 profile、挂插件树，几十秒里进程已经在跑；漏掉这段窗口就等于在用户桌面上
/// 留一个看不见的 node。启动中的句柄在启动流程手里（它要读 stdout 拿带令牌的地址），
/// 状态里只登记 pid，收尾按 pid 收掉整组。
#[derive(Default)]
pub struct DshHost {
    slot: Mutex<Slot>,
    /// 正在打开：防连点造成两个 dsh 进程（后一个会覆盖句柄，前一个变孤儿）
    opening: AtomicBool,
    /// 收尾请求计数：每次 [`DshHost::kill`] 自增，进行中的启动据此发现自己已作废
    /// （见 [`prepare_and_launch`]）—— 比「先复位标志再等」少一个竞态窗口
    epoch: AtomicU64,
}

#[derive(Default)]
enum Slot {
    #[default]
    Idle,
    /// 进程已起、就绪地址未到。只有 pid：句柄在启动流程手里
    Starting {
        pid: u32,
    },
    Ready(Running),
}

struct Running {
    child: Child,
    url: String,
}

/// 「现在是什么状况」——命令层只读这一眼，拿不到锁时按 Idle 处理
enum Snapshot {
    Idle,
    Starting,
    Ready(String),
}

impl DshHost {
    /// 收尾：就绪的收整组，启动中的按 pid 收。幂等，可重复调用。
    ///
    /// 先涨 epoch 再收进程：反过来的话，启动流程可能刚把新进程登记进去就被这一步
    /// 漏掉。涨完 epoch，进行中的启动即使晚一步登记，也会在 [`wait_for_url`] 里
    /// 发现自己已作废并自收。
    fn kill(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        let Ok(mut slot) = self.slot.lock() else {
            return;
        };
        match std::mem::replace(&mut *slot, Slot::Idle) {
            Slot::Ready(mut r) => kill_tree(&mut r.child),
            // 句柄在启动流程手里：按 pid 收整组，那边看到进程没了会自己清登记
            Slot::Starting { pid } => kill_tree_pid(pid),
            Slot::Idle => {}
        }
    }

    /// 现在的状况（顺带把「进程已经退出」的就绪句柄清掉并 reap）
    fn snapshot(&self) -> Snapshot {
        let Ok(mut slot) = self.slot.lock() else {
            return Snapshot::Idle;
        };
        // 先看就绪的那个还活着没；这一段借用 slot 内部，清了才能再动 slot
        let dead = match &mut *slot {
            Slot::Ready(r) => !matches!(r.child.try_wait(), Ok(None)),
            _ => false,
        };
        if dead {
            if let Slot::Ready(mut r) = std::mem::replace(&mut *slot, Slot::Idle) {
                let _ = r.child.wait();
            }
        }
        match &*slot {
            Slot::Idle => Snapshot::Idle,
            Slot::Starting { .. } => Snapshot::Starting,
            Slot::Ready(r) => Snapshot::Ready(r.url.clone()),
        }
    }
}

/// 起启动互斥：无论成功失败都要把 opening 放回去（`?` 提前返回时靠 Drop）。
struct OpenGuard<'a>(&'a AtomicBool);

impl Drop for OpenGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

pub fn init(app: &AppHandle) {
    app.manage(DshHost::default());
    // 上次没走完收尾留下的 dsh（崩溃 / 强杀 / 开发时重建）：后台清一次。
    // 起进程 + 等它退，别堵住 setup
    let _ = std::thread::Builder::new()
        .name("dsh-reap".into())
        .spawn(reap_orphans);
}

/// 窗口销毁 / 应用退出：把子进程收掉，别在用户桌面上留看不见的 node。
pub fn shutdown(app: &AppHandle) {
    if let Some(host) = app.try_state::<DshHost>() {
        host.kill();
    }
}

// ---------------------------------------------------------------- 收进程

/// 子进程一律放进它自己的进程组。
///
/// 收尾时按组杀，dsh 自己 spawn 出来的 node / pnpm / 工具子进程才会跟着一起走 ——
/// 只杀直接子进程会留下一串看不见的孤儿（跑一条命令、装一个插件包都会起子进程）。
/// 放在 [`cli_command`] 里：`dsh web`、`dsh plugin add`、`dsh --version` 都受益。
#[cfg(unix)]
fn in_own_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

/// 非 Unix 不设组：Windows 靠 `taskkill /T` 按父子关系清树（见 [`kill_tree_pid`]）
#[cfg(not(unix))]
fn in_own_group(_cmd: &mut Command) {}

/// 收掉 `pid` 对应的整棵进程树（进程是组长：spawn 时进了自己的组）。
///
/// 先 SIGTERM 给 dsh 收尾的机会（放掉 profile 写锁、带走自己的子进程），宽限期
/// 内没退出再 SIGKILL 兜底。**按组发**而不是只杀直接子进程，否则子进程全成孤儿。
/// SIGTERM →（宽限期内等到「没了」就收手）→ SIGKILL。
///
/// `target` 是信号对象：负 pid = 整个进程组，正 pid = 单个进程。`still_there` 问
/// 「还在不在」—— 组的问法与单个进程一样（`kill(target, 0)`），差别只在 target。
#[cfg(unix)]
fn terminate(target: i32, mut still_there: impl FnMut() -> bool) {
    unsafe { libc::kill(target, libc::SIGTERM) };
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline {
        if !still_there() {
            return; // 已经收干净，不必再上 SIGKILL
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    unsafe { libc::kill(target, libc::SIGKILL) };
}

/// 收掉 `pid` 对应的整棵进程树**并 reap**（句柄在手时用这个：能顺手把僵尸收掉，
/// 否则僵尸会让「组里还有人」的探测一直为真，白等满宽限期）
fn kill_tree(child: &mut Child) {
    let pid = child.id();
    #[cfg(unix)]
    {
        let group = -(pid as i32); // 负 pid = 进程组
        terminate(group, || {
            let _ = child.try_wait();
            unsafe { libc::kill(group, 0) == 0 }
        });
    }
    #[cfg(windows)]
    win_kill_tree(pid);
    // 兜底：组信号没送达时至少杀掉直接子进程
    let _ = child.kill();
    let _ = child.wait();
}

/// 只有 pid、没有句柄时（启动中就被收尾）：按 pid 收整组
fn kill_tree_pid(pid: u32) {
    #[cfg(unix)]
    {
        let group = -(pid as i32);
        terminate(group, || unsafe { libc::kill(group, 0) == 0 });
    }
    #[cfg(windows)]
    win_kill_tree(pid);
}

/// Windows 上按父子关系清整棵树（没有进程组的概念）
#[cfg(windows)]
fn win_kill_tree(pid: u32) {
    // /T 清整棵树，/F 不给它拖延的机会；GUI 应用里别闪一个控制台
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    use std::os::windows::process::CommandExt;
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

// ---------------------------------------------------------------- 遗留回收

/// ps 里认出来的一条「上次遗留的 dsh」
struct Leftover {
    pid: i32,
    /// 它自己就是组长（pgid == pid）：整组一起收；否则只杀这一个 —— 别去动
    /// 它可能继承来的别的进程组（可能是用户终端的前台组）
    own_group: bool,
}

/// 这条命令行是不是本应用的 dsh 实例：`dsh` 与 `--profile wallpallperem` 都要看到。
///
/// 只认一个都会误伤：`dsh` 是用户自己也可能在跑的命令，而 profile 名只是一个普通
/// 目录名（`dsh plugin --profile wallpallperem add …` 这类辅助命令同样命中 —— 它们
/// 也是本应用起的，一并回收）。
fn is_our_dsh(cmd: &str) -> bool {
    if !cmd.contains("dsh") {
        return false;
    }
    cmd.split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|w| w[0] == "--profile" && w[1] == PROFILE)
}

/// `ps -A -o pid=,ppid=,pgid=,uid=,command=` 的输出 → 要回收的遗留实例。
///
/// 只认**孤儿**（ppid=1）：命令行带这个 profile 的进程如果是用户自己在终端里开的
/// （ppid 是 shell），不归我们管，绝不能碰。抽成纯函数便于测试。
fn orphan_instances(ps_out: &str, euid: u32) -> Vec<Leftover> {
    ps_out
        .lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let pid = f.next()?.parse::<i32>().ok()?;
            let ppid = f.next()?.parse::<i32>().ok()?;
            let pgid = f.next()?.parse::<i32>().ok()?;
            let uid = f.next()?.parse::<u32>().ok()?;
            if uid != euid || ppid != 1 || pid <= 0 {
                return None;
            }
            if !is_our_dsh(&f.collect::<Vec<_>>().join(" ")) {
                return None;
            }
            Some(Leftover {
                pid,
                own_group: pgid == pid,
            })
        })
        .collect()
}

/// 上次运行没走完收尾（崩溃 / 强杀 / 开发重建）会留下失联的 dsh：它不归任何窗口
/// 管，不主动捞就永远留在用户的进程表里（每失败一次留一个）。
///
/// 非 Unix 不做：Windows 上拿「命令行 + 父进程」只有 WMI/PowerShell 一路，代价和
/// 风险都高；那边正常退出的每条路径照样收进程，只有强杀会漏。
fn reap_orphans() {
    #[cfg(unix)]
    {
        let euid = unsafe { libc::geteuid() };
        // -ww：不加宽度限制。不带它 ps 会按终端宽度截断命令行，长路径的 node
        // 命令行被截掉尾巴就认不出来了
        let Ok(out) = Command::new("ps")
            .args(["-A", "-ww", "-o", "pid=,ppid=,pgid=,uid=,command="])
            .stdin(Stdio::null())
            .output()
        else {
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        for l in orphan_instances(&text, euid) {
            // 自己是组长的按整组收（把它的子进程一起带走），否则只杀这一个
            let target = if l.own_group { -l.pid } else { l.pid };
            let scope = if l.own_group { "（整组）" } else { "" };
            tracing::info!("回收上次遗留的 dsh 进程（pid {}{scope}）", l.pid);
            terminate(target, || unsafe { libc::kill(target, 0) == 0 });
        }
    }
}

// ---------------------------------------------------------------- 环境扫描

fn dsh_home() -> PathBuf {
    if let Ok(v) = std::env::var("DSH_HOME") {
        let t = v.trim();
        if !t.is_empty() {
            return PathBuf::from(t);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".dsh")
}

pub(crate) fn profile_dir() -> PathBuf {
    dsh_home().join("profiles").join(PROFILE)
}

#[cfg(unix)]
fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.is_file()
        && std::fs::metadata(p)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(p: &Path) -> bool {
    p.is_file()
}

/// GUI 启动的进程 PATH 常常只有 `/usr/bin:/bin`（macOS 尤其），用户在
/// `~/.zshrc` 里配的 npm 全局目录一个都看不见 —— 所以除了 PATH 还要扫常见位置。
fn path_candidates(name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(paths) = std::env::var_os("PATH") {
        out.extend(std::env::split_paths(&paths).map(|d| d.join(name)));
    }
    for d in common_tool_dirs() {
        out.push(d.join(name));
    }
    out
}

/// GUI 启动的应用最常缺的那几个 bin 目录。同一份清单干两件事：
/// **去哪找 dsh/node/pnpm**，以及**给子进程的 PATH 补什么**。
///
/// 两件事都必要：dsh 常见装法就是 `npm i -g @deepseek-ai/dsh` —— 全局 bin 落在
/// nvm/volta/Homebrew 的 node 目录里，GUI 进程的精简 PATH 一个都看不见；而
/// `dsh plugin … add` 还要再去调 **pnpm**（macOS 上 pnpm 的默认 PNPM_HOME 是
/// `~/Library/pnpm`），漏一个就是「明明装了却说没装」。
fn common_tool_dirs() -> Vec<PathBuf> {
    tool_dirs(dirs::home_dir().as_deref())
}

/// [`common_tool_dirs`] 的纯函数版本（家目录可注入，便于测试）
fn tool_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = home {
        dirs.push(home.join(".npm-global/bin"));
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".bun/bin"));
        dirs.push(home.join(".volta/bin"));
        dirs.push(home.join(".yarn/bin"));
        // pnpm 在 macOS 的默认 PNPM_HOME；Linux 常见是 ~/.local/share/pnpm
        dirs.push(home.join("Library/pnpm"));
        dirs.push(home.join(".local/share/pnpm"));
        // nvm / fnm 的版本目录：node 与 `npm i -g` 装的 dsh/pnpm 都住在这里
        dirs.extend(node_version_bins(home));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/usr/bin"));
    dirs.push(PathBuf::from("/snap/bin"));
    dirs
}

/// nvm / fnm 各版本自带的 bin，新版本在前。
///
/// 关键点：这些目录里不只有 node —— `npm i -g` 装的 dsh、pnpm 也在这里。早期实现只
/// 用它找 node，结果「本机装了 dsh」被判成没装。
fn node_version_bins(home: &Path) -> Vec<PathBuf> {
    let mut versions: Vec<PathBuf> = Vec::new();
    for root in [
        home.join(".nvm/versions/node"),
        home.join(".local/share/fnm/node-versions"),
        home.join(".fnm/node-versions"),
    ] {
        if let Ok(entries) = std::fs::read_dir(&root) {
            versions.extend(entries.flatten().map(|e| e.path()));
        }
    }
    // 按版本号排，不按字符串：`v1.10.0` 必须排在 `v1.2.3` 之后
    versions.sort_by_key(|p| {
        version_key(
            &p.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
        )
    });
    versions.reverse();
    versions.into_iter().map(|v| v.join("bin")).collect()
}

/// 版本目录名的排序键（数字段，缺段补 0）
fn version_key(name: &str) -> (u64, u64, u64) {
    let core = name
        .trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or("");
    let mut nums = [0u64; 3];
    for (i, part) in core.split('.').take(3).enumerate() {
        nums[i] = part
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap_or(0);
    }
    (nums[0], nums[1], nums[2])
}

/// 找 `dsh` CLI。返回 (可执行文件, 来源标签)；来源只用于界面展示。
fn find_cli() -> Option<(PathBuf, &'static str)> {
    let names: &[&str] = if cfg!(windows) {
        &["dsh.cmd", "dsh.exe", "dsh"]
    } else {
        &["dsh"]
    };
    let in_path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for name in names {
        for cand in path_candidates(name) {
            if !is_executable_file(&cand) {
                continue;
            }
            let source = if cand
                .parent()
                .map(|d| in_path.iter().any(|x| x == d))
                .unwrap_or(false)
            {
                "path"
            } else {
                "global"
            };
            return Some((cand, source));
        }
    }
    None
}

fn find_node() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["node.exe", "node"]
    } else {
        &["node"]
    };
    for name in names {
        for cand in path_candidates(name) {
            if is_executable_file(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

/// 找 `pnpm`：与 dsh/node 同一套候选目录（nvm/fnm 的版本 bin、`~/Library/pnpm` 等）
fn find_pnpm() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["pnpm.cmd", "pnpm.exe", "pnpm"]
    } else {
        &["pnpm"]
    };
    for name in names {
        for cand in path_candidates(name) {
            if is_executable_file(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

/// 找 `npm`：候选目录优先，再退到「node 旁边」—— 自带 node 的安装方式里 npm 就在那儿
fn find_npm(node: Option<&Path>) -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["npm.cmd", "npm.exe", "npm"]
    } else {
        &["npm"]
    };
    for name in names {
        for cand in path_candidates(name) {
            if is_executable_file(&cand) {
                return Some(cand);
            }
        }
    }
    let dir = node?.parent()?;
    names
        .iter()
        .map(|n| dir.join(n))
        .find(|p| is_executable_file(p))
}

/// `npm install -g pnpm`。
///
/// 复用 [`cli_command`]：POSIX 上的 `npm` 是指向 `npm-cli.js` 的软链（直接 node 跑，
/// 绕开 `#!/usr/bin/env node` 对 PATH 的依赖），Windows 的 `npm.cmd` 走 `cmd /C`；
/// 子进程 PATH 也一并补齐（npm 自己要能找到 node）。
fn pnpm_install_command(npm: &Path, node: Option<&Path>) -> Command {
    let mut cmd = cli_command(npm, node);
    cmd.arg("install").arg("--global").arg("pnpm");
    cmd
}

/// 全局装 pnpm。阻塞（npm + 联网），调用方放进 spawn_blocking。
fn install_pnpm() -> Result<Value, PluginError> {
    let node = find_node();
    let npm = find_npm(node.as_deref()).ok_or_else(|| PluginError::code("plugin-npm-missing"))?;
    tracing::info!("installing pnpm via {}", npm.display());
    let mut cmd = pnpm_install_command(&npm, node.as_deref());
    // 工作目录用临时目录：npm 会在 cwd 里找 package.json，别让用户的工程影响这次全局安装
    cmd.current_dir(std::env::temp_dir());
    let out = run_capped(cmd, PNPM_INSTALL_TIMEOUT)?;
    if out.timed_out {
        return Err(PluginError::new("plugin-pnpm-install", "timeout"));
    }
    if out.code != Some(0) {
        return Err(PluginError::new(
            "plugin-pnpm-install",
            format!("exit {:?}\n{}\n{}", out.code, out.stdout, out.stderr),
        ));
    }
    // 装完再找一次：npm 的全局 bin 就在候选目录里，正常立刻能找到；找不到也别当失败 ——
    // 有些安装方式把 bin 放在我们要等到下次启动才看得见的地方
    match find_pnpm() {
        Some(p) => {
            tracing::info!("pnpm installed at {}", p.display());
            Ok(json!({
                "installed": true,
                "pnpmPath": p.to_string_lossy(),
                "npmPath": npm.to_string_lossy(),
            }))
        }
        None => Ok(json!({
            "installed": false,
            "npmPath": npm.to_string_lossy(),
            "hint": "not-found-after-install",
        })),
    }
}

/// 桌面版 DeepSeek Harness：**不含命令行**，只用于引导文案（「你装了桌面版，
/// 但插件需要 CLI」）与「打开桌面版」。
fn find_desktop_app() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        cands.push(PathBuf::from("/Applications/DeepSeek Harness.app"));
        if let Some(home) = dirs::home_dir() {
            cands.push(home.join("Applications/DeepSeek Harness.app"));
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            cands
                .push(PathBuf::from(&local).join("Programs/DeepSeek Harness/DeepSeek Harness.exe"));
            cands.push(PathBuf::from(&local).join("DeepSeek Harness/DeepSeek Harness.exe"));
        }
    }
    #[cfg(target_os = "linux")]
    {
        cands.push(PathBuf::from("/opt/DeepSeek Harness/deepseek-harness"));
        cands.push(PathBuf::from("/usr/local/bin/deepseek-harness"));
        if let Some(home) = dirs::home_dir() {
            cands.push(home.join(".local/bin/deepseek-harness"));
            cands.push(home.join("Applications/DeepSeek Harness.AppImage"));
        }
    }
    cands.into_iter().find(|p| p.exists())
}

/// 组装子进程的 PATH：node 与 cli 所在目录排最前，其余继承。
///
/// 必要而不是保险 —— GUI 应用 spawn 出来的进程继承的是精简 PATH，而 `dsh` 自己
/// 还要 spawn node/npx/pnpm 才能起来，找不到 node 就是启动即失败。
fn child_path(cli: &Path, node: Option<&Path>) -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(n) = node.and_then(|n| n.parent()) {
        dirs.push(n.to_path_buf());
    }
    if let Some(d) = cli.parent() {
        dirs.push(d.to_path_buf());
    }
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    // 常见工具目录（pnpm/node 的几种装法）补在后面：`dsh plugin` 要能找到 pnpm
    dirs.extend(common_tool_dirs());
    dirs.dedup();
    std::env::join_paths(dirs).unwrap_or_default()
}

/// 构造运行 dsh 的 Command。
///
/// CLI 若是 JS 入口（npm 全局安装的 `bin/dsh` 就是指向 `lib/bin.js` 的软链），
/// 直接 `node <入口>` 而**不是**靠 `#!/usr/bin/env node` —— 后者的前提是 node 在
/// PATH 里，而这正是 GUI 启动环境最不可靠的一点。
fn cli_command(cli: &Path, node: Option<&Path>) -> Command {
    // 优先用 **CLI 自己目录里的 node**：nvm / volta 这类按版本切的装法里，dsh 就是
    // 用那个 node 装的；退而求其次才用全局探测到的 node
    let sibling = cli
        .parent()
        .map(|d| d.join(if cfg!(windows) { "node.exe" } else { "node" }))
        .filter(|p| is_executable_file(p));
    let node = sibling.as_deref().or(node);
    let resolved = std::fs::canonicalize(cli).unwrap_or_else(|_| cli.to_path_buf());
    let mut cmd = if resolved.extension().and_then(|e| e.to_str()) == Some("js") {
        match node {
            Some(n) => {
                let mut c = Command::new(n);
                c.arg(&resolved);
                c
            }
            None => Command::new(cli),
        }
    } else if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(cli);
        c
    } else {
        Command::new(cli)
    };
    cmd.env("PATH", child_path(cli, node));
    cmd.stdin(Stdio::null());
    // 自己的进程组：收尾时按组杀，dsh 起的子进程跟着一起走（见 kill_tree_pid）
    in_own_group(&mut cmd);
    cmd
}

/// `dsh --version`。探测超时（10s）就放弃版本号，不阻塞扫描。
///
/// 超时必须把进程收掉再走：读输出是阻塞的，进程不退出的话探测线程会永远挂着
/// （每次扫描漏一个线程 + 一个 node），所以超时后杀组并 join 回来。
fn probe_version(cli: &Path, node: Option<&Path>) -> Option<String> {
    probe_version_within(cli, node, PROBE_TIMEOUT)
}

/// [`probe_version`] 的可注入超时版本（测试用）；返回前保证线程与进程都已收场
fn probe_version_within(cli: &Path, node: Option<&Path>, timeout: Duration) -> Option<String> {
    let mut cmd = cli_command(cli, node);
    cmd.arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().ok()?;
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    let reader = match std::thread::Builder::new()
        .name("dsh-probe".into())
        .spawn(move || {
            // 退出后读干净管道（进程已死时这里立刻返回）
            let _ = tx.send(child.wait_with_output());
        }) {
        Ok(h) => h,
        Err(_) => {
            // 线程起不来就没有读者：按 pid 把进程收掉（句柄随闭包一起没了）
            kill_tree_pid(pid);
            return None;
        }
    };
    let out = match rx.recv_timeout(timeout) {
        Ok(out) => out,
        Err(_) => {
            // 超时：先收进程，读线程自然结束 —— 不是简单地把它扔下不管
            kill_tree_pid(pid);
            let _ = reader.join();
            tracing::warn!("dsh --version 超时（已收回进程），本次扫描不给版本号");
            return None;
        }
    };
    let _ = reader.join();
    let o = out.ok()?;
    if !o.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        // 多行输出只取第一行（版本号那行）
        Some(s.lines().next().unwrap_or("").trim().to_string())
    }
}

// ---------------------------------------------------------------- profile

/// 干净 profile 的 manifest，结构/键序/缩进与 dsh `initProfile()` 一致。
const PROFILE_MANIFEST_TEMPLATE: &str = r#"{
  "name": "dsh-profile-__PROFILE__",
  "private": true,
  "dependencies": {},
  "dsh": {
    "profile": {
      "bundles": [
        "@deepseek-ai/dsh-base",
        "@deepseek-ai/dsh-web-app"
      ]
    }
  }
}
"#;

/// dsh 自带的 pnpm 配置（原样抄 dsh 的 PROFILE_PNPM_WORKSPACE，profile 首次
/// 初始化时写一次，之后不再动）
const PROFILE_PNPM_WORKSPACE: &str = "packages:
  - .

nodeLinker: hoisted
autoInstallPeers: false
";

/// dsh 自带 patch 模板的原文：用来判断「这个 profile 的 patch 层还是初始状态」。
const PROFILE_PATCH_TEMPLATE: &str =
    "# Your patch layer for this dsh profile, applied after every bundle layer:
# a top-level YAML array of loader patch entries (id-targeted config
# overrides, disables, and insert lists; `!!js` expressions allowed).
[]
";

/// 干净 profile 期望的 bundle 清单（与 manifest 模板同源，用于识别「已被用户改过」）
const CLEAN_BUNDLES: [&str; 2] = ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"];

fn profile_manifest() -> String {
    PROFILE_MANIFEST_TEMPLATE.replace("__PROFILE__", PROFILE)
}

/// profile 的 MCP patch 层正文。
///
/// `insert` 而不是覆盖某一行：mcp-client 在随附 profile 里默认没有启用行，追加是
/// 唯一不需要知道原树形状的做法（见 dsh-mcp-client 的 README §最小配置）。
fn mcp_patch_body(mcp_url: &str) -> String {
    format!(
        "# Managed by the WallpaperEM plugin page (built-in plugin: DeepSeek Harness).\n\
         # Rewritten on every open with the current MCP port and token; local edits are lost.\n\
         # For your own overlays use cordis.patch.yml, or pass dsh --patch <file>.\n\
         {PATCH_MARKER}\n\
         - insert:\n\
         \x20   - id: {MCP_SERVER}-mcp\n\
         \x20     name: '@deepseek-ai/dsh-mcp-client'\n\
         \x20     config:\n\
         \x20       serverName: {MCP_SERVER}\n\
         \x20       transport: streamable-http\n\
         \x20       url: '{mcp_url}'\n\
         \x20       failOnStartupError: false\n"
    )
}

/// profile 的准备结果：patch 层写在哪、是否需要 --patch 挂载。
struct ProfilePlan {
    overlay: Option<PathBuf>,
}

fn write_file(path: &Path, body: &str) -> Result<(), PluginError> {
    std::fs::write(path, body).map_err(|e| PluginError::new("profile-write", e))
}

/// 准备干净 profile + MCP 层。
///
/// 只碰三个托管文件（缺失时才建）：`package.json`、`pnpm-workspace.yaml`、
/// `cordis.patch.yml` —— 与 dsh 的 `initProfile()` 语义一致：**已存在的一律不动**。
/// MCP 层优先写进 `cordis.patch.yml`（当它是初始状态或本应用写的）；一旦发现用户
/// 自己改过，就退到独立 overlay 文件，启动时用 `--patch` 挂载。
fn ensure_profile(dir: &Path, mcp_url: &str) -> Result<ProfilePlan, PluginError> {
    std::fs::create_dir_all(dir).map_err(|e| PluginError::new("profile-write", e))?;

    let manifest = dir.join("package.json");
    if !manifest.exists() {
        write_file(&manifest, &profile_manifest())?;
    }
    let workspace = dir.join("pnpm-workspace.yaml");
    if !workspace.exists() {
        write_file(&workspace, PROFILE_PNPM_WORKSPACE)?;
    }

    let body = mcp_patch_body(mcp_url);
    let patch = dir.join(PATCH_FILE);
    let existing = std::fs::read_to_string(&patch).unwrap_or_default();
    let ours = existing.trim().is_empty()
        || existing.contains(PATCH_MARKER)
        || existing.trim() == PROFILE_PATCH_TEMPLATE.trim();
    if ours {
        write_file(&patch, &body)?;
        return Ok(ProfilePlan { overlay: None });
    }
    let overlay = dir.join(OVERLAY_FILE);
    write_file(&overlay, &body)?;
    Ok(ProfilePlan {
        overlay: Some(overlay),
    })
}

/// 该 profile 是否还是「干净」的（bundle 清单与我们的模板一致）。只读检查，不修。
fn profile_is_clean(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("package.json")) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let bundles = v
        .get("dsh")
        .and_then(|d| d.get("profile"))
        .and_then(|p| p.get("bundles"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    bundles.len() == CLEAN_BUNDLES.len()
        && bundles
            .iter()
            .zip(CLEAN_BUNDLES.iter())
            .all(|(a, b)| a.as_str() == Some(*b))
}

/// MCP 层当前是否已是「指向这个地址」的最新版。
fn mcp_layer_current(dir: &Path, mcp_url: &str) -> bool {
    let body = mcp_patch_body(mcp_url);
    ["cordis.patch.yml", OVERLAY_FILE].iter().any(|name| {
        std::fs::read_to_string(dir.join(name))
            .map(|t| t == body)
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------- 启动

struct Started {
    url: String,
    port: u16,
}

/// 拿一个空闲端口交给 dsh（`--port`），省得它默认端口被占时启动失败。
///
/// bind(0) 拿到端口后立刻释放，中间的理论竞态窗口极小；真被抢了 dsh 会启动失败，
/// 报错里会带上原因。
fn free_port() -> Result<u16, PluginError> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| PluginError::new("dsh-port", e))?;
    let port = l
        .local_addr()
        .map_err(|e| PluginError::new("dsh-port", e))?
        .port();
    drop(l);
    Ok(port)
}

fn spawn_dsh(
    cli: &Path,
    node: Option<&Path>,
    overlay: Option<&Path>,
    port: u16,
    cwd: &Path,
) -> Result<Child, PluginError> {
    let mut cmd = cli_command(cli, node);
    cmd.arg("--profile").arg(PROFILE);
    if let Some(p) = overlay {
        cmd.arg("--patch").arg(p);
    }
    // `--no-open`：浏览器由我们自己的窗口替代（外部页面开两次是噪音）。
    // 注意顺序：`--profile` 与 `--patch` 是启动器的 flag，`--no-open/--port` 是
    // web 应用的 flag，必须落在后面（见 dsh README §应用参数）。
    cmd.arg("--no-open").arg("--port").arg(port.to_string());
    // 工作目录 = profile 目录：会话的默认 workspace 就落在这里，agent 造出来的
    // 东西不会跑进用户的项目目录
    cmd.current_dir(cwd);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.spawn().map_err(|e| PluginError::new("dsh-spawn", e))
}

/// 等 dsh 打印就绪行 `dsh web: <url>`。
///
/// 必须用它的地址而不是自己拼 `http://127.0.0.1:<port>/`：那行的 URL 带本次进程
/// 的令牌，浏览器先拿签名 cookie 再跳转；不带令牌访问根路径拿不到会话。
///
/// `epoch` 是启动开始时的收尾计数：期间窗口关 / 点停止 / 应用退出都会让它变，
/// 这时立刻收掉进程并按 `dsh-cancelled` 返回 —— 不能继续等满 90s，更不能把
/// 一个已经死掉的进程当成「就绪」登记回去。**返回前保证子进程已收场**。
fn wait_for_url(
    child: &mut Child,
    port: u16,
    host: &DshHost,
    epoch: u64,
) -> Result<String, PluginError> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| PluginError::code("dsh-no-stdout"))?;
    let stderr = child.stderr.take();
    let (tx, rx): (_, Receiver<String>) = mpsc::channel();

    let _ = std::thread::Builder::new()
        .name("dsh-stdout".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                tracing::info!("dsh: {line}");
                if let Some(rest) = line.strip_prefix("dsh web: ") {
                    let url = rest.split_whitespace().next().unwrap_or("");
                    if url.starts_with("http") {
                        let _ = tx.send(url.to_string());
                    }
                }
            }
        });

    // stderr 只留最后 40 行：报错时够定位，又不至于把日志灌进窗口
    let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(err) = stderr {
        let buf = tail.clone();
        let _ = std::thread::Builder::new()
            .name("dsh-stderr".into())
            .spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    tracing::warn!("dsh: {line}");
                    if let Ok(mut b) = buf.lock() {
                        if b.len() >= 40 {
                            b.pop_front();
                        }
                        b.push_back(line);
                    }
                }
            });
    }
    let collect = |buf: &Arc<Mutex<VecDeque<String>>>| {
        buf.lock()
            .map(|b| b.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    };

    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        // 收尾请求（关窗 / 停止 / 退出）：本次启动作废，收掉进程立刻返回
        if host.epoch.load(Ordering::SeqCst) != epoch {
            kill_tree(child);
            return Err(PluginError::code("dsh-cancelled"));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left.min(Duration::from_millis(300))) {
            Ok(url) => return Ok(url),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // 进程提前退出（模型没配、前端没构建、端口被抢…）：带着 stderr 回去
                if let Ok(Some(status)) = child.try_wait() {
                    let _ = child.wait();
                    return Err(PluginError::new(
                        "dsh-exited",
                        format!("{status}\n{}", collect(&tail)),
                    ));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let cancelled = host.epoch.load(Ordering::SeqCst) != epoch;
    kill_tree(child);
    if cancelled {
        return Err(PluginError::code("dsh-cancelled"));
    }
    // 端口写进诊断：超时最常见的原因就是端口这块出问题（被别的进程抢了/防火墙）
    Err(PluginError::new(
        "dsh-timeout",
        format!("port {port}\n{}", collect(&tail)),
    ))
}

/// 阻塞部分：准备 profile → （复用或）启动 → 拿到 URL。放进 spawn_blocking 跑。
fn prepare_and_launch(app: &AppHandle, mcp_url: &str) -> Result<Started, PluginError> {
    let (cli, source) = find_cli().ok_or_else(|| PluginError::code("dsh-missing"))?;
    tracing::info!("dsh cli: {} ({source})", cli.display());
    let node = find_node();
    let dir = profile_dir();
    let plan = ensure_profile(&dir, mcp_url)?;

    let host = app.state::<DshHost>();
    match host.snapshot() {
        // 已经有一个跑着的：复用它（重启会丢掉用户正在进行的会话）
        Snapshot::Ready(url) => return Ok(Started { url, port: 0 }),
        // 上一个还在启动：等它的结果，别再起一个
        Snapshot::Starting => return Err(PluginError::code("dsh-starting")),
        Snapshot::Idle => {}
    }
    // 启动开始时的收尾计数：期间任何一次 kill（关窗 / 停止 / 退出）都会让它变
    let epoch = host.epoch.load(Ordering::SeqCst);

    let port = free_port()?;
    let mut child = spawn_dsh(&cli, node.as_deref(), plan.overlay.as_deref(), port, &dir)?;
    let pid = child.id();
    // 先登记 pid 再等就绪：冷启动要建 profile、挂插件树，几十秒里进程已经在跑，
    // 这段窗口里关窗 / 退出也必须能把它收掉
    match host.slot.lock() {
        Ok(mut slot) => *slot = Slot::Starting { pid },
        Err(_) => {
            kill_tree(&mut child);
            return Err(PluginError::code("dsh-store"));
        }
    }

    match wait_for_url(&mut child, port, &host, epoch) {
        Ok(url) => {
            let mut slot = match host.slot.lock() {
                Ok(slot) => slot,
                Err(_) => {
                    kill_tree(&mut child);
                    return Err(PluginError::code("dsh-store"));
                }
            };
            // 启动期间被收尾过：这次启动已经作废（进程也已经被收掉了），
            // 别把一个死进程登记成「就绪」
            if host.epoch.load(Ordering::SeqCst) != epoch {
                drop(slot);
                kill_tree(&mut child);
                return Err(PluginError::code("dsh-cancelled"));
            }
            *slot = Slot::Ready(Running {
                child,
                url: url.clone(),
            });
            Ok(Started { url, port })
        }
        Err(e) => {
            // 清登记（还写着我这一个 pid 时才清）；进程由 wait_for_url 收场
            if let Ok(mut slot) = host.slot.lock() {
                if matches!(&*slot, Slot::Starting { pid: p } if *p == pid) {
                    *slot = Slot::Idle;
                }
            }
            // 被收尾打断的启动不算「起不来」：报专用码，界面能说清原因
            if host.epoch.load(Ordering::SeqCst) != epoch {
                return Err(PluginError::code("dsh-cancelled"));
            }
            Err(e)
        }
    }
}

// ---------------------------------------------------------------- 命令

/// 扫描本机 dsh 环境 + 这个插件的 profile 状态。前端据此决定「打开」还是「引导安装」。
#[tauri::command]
pub async fn dsh_scan(app: AppHandle) -> Value {
    let home = dsh_home();
    let dir = home.join("profiles").join(PROFILE);
    let (cli, node, version) = tauri::async_runtime::spawn_blocking(move || {
        let cli = find_cli();
        let node = find_node();
        let version = cli
            .as_ref()
            .and_then(|(c, _)| probe_version(c, node.as_deref()));
        (cli, node, version)
    })
    .await
    .unwrap_or((None, None, None));

    let mcp = crate::mcp::mcp_status(app.clone());
    let mcp_url = mcp
        .get("urlWithToken")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let running = app
        .try_state::<DshHost>()
        .map(|h| h.snapshot())
        .and_then(|s| match s {
            Snapshot::Ready(url) => Some(url),
            // 启动中不算「运行中」：界面上那个「打开」按钮会带着
            // dsh-starting 提示自己等一下
            Snapshot::Starting | Snapshot::Idle => None,
        })
        .unwrap_or_default();

    json!({
        "profile": PROFILE,
        "dshHome": home.to_string_lossy(),
        "profileDir": dir.to_string_lossy(),
        "profileInitialized": dir.join("package.json").exists(),
        "profileClean": profile_is_clean(&dir),
        "mcpInjected": !mcp_url.is_empty() && mcp_layer_current(&dir, &mcp_url),
        "installed": cli.is_some(),
        "cliPath": cli.as_ref().map(|(c, _)| c.to_string_lossy().to_string()),
        "cliSource": cli.as_ref().map(|(_, s)| *s),
        "version": version,
        "desktopApp": find_desktop_app().map(|p| p.to_string_lossy().to_string()),
        "nodePath": node.as_ref().map(|n| n.to_string_lossy().to_string()),
        "npmPath": find_npm(node.as_deref()).map(|p| p.to_string_lossy().to_string()),
        // pnpm 只有 dsh-profile 型插件才需要（`dsh plugin … add` 靠它装包）
        "pnpmPath": find_pnpm().map(|p| p.to_string_lossy().to_string()),
        "installCommand": INSTALL_COMMAND,
        "downloadUrl": DOWNLOAD_URL,
        "running": !running.is_empty(),
        "url": if running.is_empty() { Value::Null } else { json!(running) },
        "mcp": {
            "serverName": MCP_SERVER,
            "enabled": mcp.get("enabled").cloned().unwrap_or(json!(false)),
            "running": mcp.get("running").cloned().unwrap_or(json!(false)),
            "url": mcp.get("url").cloned().unwrap_or(Value::Null),
        },
    })
}

/// 一键装 pnpm：`dsh-profile` 型插件要往 profile 装包，`dsh plugin` 需要 pnpm。
///
/// 会真的改动用户的 Node 全局环境（`npm install -g pnpm`），所以界面必须先确认再调。
#[tauri::command]
pub async fn dsh_install_pnpm() -> Result<Value, PluginError> {
    tauri::async_runtime::spawn_blocking(install_pnpm)
        .await
        .map_err(|e| PluginError::new("plugin-pnpm-install", e))?
}

/// 打开内置插件：准备 profile + 嵌入 MCP + 启动 dsh，然后**在新窗口里**打开它的页面。
///
/// 已经开着窗口且进程还活着时只聚焦，不重启（重启会丢掉用户正在进行的会话）。
#[tauri::command]
pub async fn dsh_open(app: AppHandle) -> Result<Value, PluginError> {
    let host = app.state::<DshHost>();
    if host.opening.swap(true, Ordering::SeqCst) {
        return Err(PluginError::code("dsh-starting"));
    }
    let _guard = OpenGuard(&host.opening);
    ensure_running(&app).await
}

/// 打开（或聚焦）dsh 窗口：装好 MCP / 干净 profile，启动 dsh，再把它的页面开成窗口。
///
/// 与 `dsh_open` 的分工：这里只做「把它弄起来」，连点保护与状态判断在外面 ——
/// 第三方 `dsh-profile` 插件（往 profile 装包的那种）也走这条路径。
pub(crate) async fn ensure_running(app: &AppHandle) -> Result<Value, PluginError> {
    let host = app.state::<DshHost>();
    let alive = match host.snapshot() {
        Snapshot::Ready(url) => Some(url),
        // 正在启动：让界面等上一次的结果，别再点一次（会起来两个）
        Snapshot::Starting => return Err(PluginError::code("dsh-starting")),
        Snapshot::Idle => None,
    };
    if let Some(w) = app.get_webview_window(PLUGIN_WINDOW_LABEL) {
        match &alive {
            Some(url) => {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
                return Ok(json!({ "url": url, "reused": true }));
            }
            // 进程没了，窗口里是死页面：关掉重开
            None => {
                let _ = w.close();
            }
        }
    }

    // MCP 是「插件能干活」的前提：没开就顺手开（用户不需要先跑去设置页点一下）
    let mcp = crate::mcp::mcp_status(app.clone());
    let mcp_enabled = mcp.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    let mcp = if mcp_enabled {
        mcp
    } else {
        crate::mcp::mcp_set_enabled(app.clone(), true)
            .map_err(|e| PluginError::new("mcp-start", e))?
    };
    let mcp_url = mcp
        .get("urlWithToken")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if mcp_url.is_empty() {
        return Err(PluginError::code("mcp-unavailable"));
    }

    let app2 = app.clone();
    let url = mcp_url.clone();
    let started = tauri::async_runtime::spawn_blocking(move || prepare_and_launch(&app2, &url))
        .await
        .map_err(|e| PluginError::new("dsh-start", e))??;

    // 开窗必须在主线程（Tauri 的窗口构建要求）；错误经 channel 带回来
    let (tx, rx) = mpsc::channel();
    let app3 = app.clone();
    let url_for_window = started.url.clone();
    app.run_on_main_thread(move || {
        let app4 = app3.clone();
        let r = open_url_window(
            &app3,
            PLUGIN_WINDOW_LABEL,
            &url_for_window,
            WINDOW_TITLE,
            move || shutdown(&app4),
        );
        let _ = tx.send(r);
    })
    .map_err(|e| PluginError::new("plugin-window", e))?;
    // 内层是开窗那步的结果（地址无效 / 建窗失败），错误文本由 window 模块给出
    rx.recv()
        .map_err(|e| PluginError::new("plugin-window", e))?
        .map_err(|e| PluginError::new("plugin-window", e))?;

    tracing::info!("dsh plugin window opened: {}", started.url);
    Ok(json!({
        "url": started.url,
        "port": started.port,
        "profileDir": profile_dir().to_string_lossy(),
        "mcpUrl": mcp_url,
    }))
}

/// 停掉后台 dsh 并关掉它的窗口。
///
/// 往 profile 装完插件必须走这一步再重开：我们的干净 profile 没开 HMR，组合包清单
/// （`dsh.profile.bundles`）是插件装好后由 dsh 的 plugin-manager 改写的，只有下次
/// 启动才会加载进去。
pub(crate) fn stop_and_close(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(PLUGIN_WINDOW_LABEL) {
        let _ = w.close();
    }
    shutdown(app);
}

/// 干净 profile 里已经装了的依赖（`dependencies` 的键）。读不到就当空。
pub(crate) fn profile_packages() -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(profile_dir().join("package.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    v.get("dependencies")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// `want` 里还没装到 profile 的包
pub(crate) fn missing_packages(want: &[String]) -> Vec<String> {
    let have = profile_packages();
    want.iter().filter(|p| !have.contains(p)).cloned().collect()
}

/// 把插件包装进 profile：`dsh plugin --profile <名> add <包…>`。
///
/// dsh 侧会用 pnpm 装，并在装完后把声明了 `dsh.bundle` 的包追加进
/// `dsh.profile.bundles`（plugin-manager 的 reconcile）—— 所以这一条命令就够，
/// 不需要我们自己去改组合包清单。慢（要联网、pnpm 还要等 profile 写锁），
/// 阻塞调用，调用方放进 spawn_blocking。
pub(crate) fn install_profile_packages(packages: &[String]) -> Result<Value, PluginError> {
    let (cli, _) = find_cli().ok_or_else(|| PluginError::code("dsh-missing"))?;
    let node = find_node();
    let mut cmd = cli_command(&cli, node.as_deref());
    cmd.arg("plugin").arg("--profile").arg(PROFILE).arg("add");
    for p in packages {
        cmd.arg(p);
    }
    cmd.current_dir(profile_dir());
    let out = run_capped(cmd, INSTALL_TIMEOUT)?;
    if out.timed_out {
        return Err(PluginError::new("plugin-pnpm", "timeout"));
    }
    if out.code != Some(0) {
        // dsh 在 pnpm 缺失时回 127 并打印提示；分开报，界面能说清「装 pnpm」
        if out.code == Some(127) {
            return Err(PluginError::new("plugin-pnpm-missing", out.stderr));
        }
        return Err(PluginError::new(
            "plugin-pnpm",
            format!("exit {:?}\n{}\n{}", out.code, out.stdout, out.stderr),
        ));
    }
    tracing::info!("已装进 dsh profile: {packages:?}");
    Ok(json!({ "packages": packages }))
}

/// 跑一条命令，收集有上限的输出，并带超时。
///
/// 与 `wait_for_url` 的读法一样：stdout/stderr 各一个读取线程（不读干净会把子进程
/// 卡在管道缓冲上），各留最后 N 行。超时则杀掉。
fn run_capped(mut cmd: Command, timeout: Duration) -> Result<RunOutput, PluginError> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| PluginError::new("plugin-pnpm", e))?;

    let out_buf: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    let err_buf: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    let mut handles = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        handles.push(drain(pipe, "dsh-out", out_buf.clone()));
    }
    if let Some(pipe) = child.stderr.take() {
        handles.push(drain(pipe, "dsh-err", err_buf.clone()));
    }

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {}
            Err(e) => return Err(PluginError::new("plugin-pnpm", e)),
        }
        if Instant::now() >= deadline {
            timed_out = true;
            // 按整棵树收：`dsh plugin add` 底下挂着 pnpm，只杀 dsh 会留下它 ——
            // 而它还攥着 stdout/stderr 管道，下面两个读线程就永远 join 不回来
            kill_tree(&mut child);
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    for h in handles {
        let _ = h.join();
    }
    let collect = |buf: &Arc<Mutex<VecDeque<String>>>| {
        buf.lock()
            .map(|b| b.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    };
    Ok(RunOutput {
        code,
        stdout: collect(&out_buf),
        stderr: collect(&err_buf),
        timed_out,
    })
}

struct RunOutput {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

/// 把一条管道读到 EOF，只留最后 [`MAX_TAIL_LINES`] 行。线程带名字：万一哪天它
/// 没退出（子进程攥着管道不放就是这种症状），看线程列表就能认出是谁。
fn drain<R: std::io::Read + Send + 'static>(
    pipe: R,
    tag: &'static str,
    buf: Arc<Mutex<VecDeque<String>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(tag.into())
        .spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                tracing::info!("{tag}: {line}");
                if let Ok(mut b) = buf.lock() {
                    if b.len() >= MAX_TAIL_LINES {
                        b.pop_front();
                    }
                    b.push_back(line);
                }
            }
        })
        .expect("spawn pipe reader")
}

/// 停掉后台的 dsh 进程（窗口由用户自己关；这里给「停止」按钮用）。
///
/// 启动中的实例同样收：点了停止还让它在后台继续把 profile 建完，就是漏一个进程。
#[tauri::command]
pub fn dsh_stop(app: AppHandle) -> Value {
    if let Some(host) = app.try_state::<DshHost>() {
        host.kill();
    }
    json!({ "ok": true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_dirs_cover_nvm_and_pnpm_homes() {
        // 用一个假家目录，把「装了却说没装」的几种装法钉住
        let home = std::env::temp_dir().join(format!("we-home-{}", std::process::id()));
        let nvm = home.join(".nvm/versions/node");
        for v in ["v1.2.3", "v1.10.0"] {
            std::fs::create_dir_all(nvm.join(v).join("bin")).unwrap();
        }
        let dirs = tool_dirs(Some(&home));
        assert!(
            dirs.contains(&home.join("Library/pnpm")),
            "pnpm 的 macOS 默认 PNPM_HOME"
        );
        assert!(dirs.contains(&home.join(".local/share/pnpm")));
        assert!(dirs.contains(&nvm.join("v1.2.3/bin")));
        assert!(dirs.contains(&nvm.join("v1.10.0/bin")));
        // 新版本在前：v1.10.0 要排在 v1.2.3 之前（按数字比，不按字符串）
        let i10 = dirs
            .iter()
            .position(|d| d.ends_with("v1.10.0/bin"))
            .unwrap();
        let i2 = dirs.iter().position(|d| d.ends_with("v1.2.3/bin")).unwrap();
        assert!(i10 < i2, "新版本应当排在前面: {dirs:?}");
        // 版本目录不存在时也不能炸
        assert!(!tool_dirs(Some(&home.join("nope"))).is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn pnpm_install_command_has_fixed_arguments() {
        // 固定参数、无用户输入：这条命令会被界面上的「安装 pnpm」直接触发
        let cmd = pnpm_install_command(Path::new("/usr/local/bin/npm"), None);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(&args[args.len() - 3..], &["install", "--global", "pnpm"]);
    }

    #[test]
    fn version_key_sorts_numerically() {
        assert!(version_key("v1.10.0") > version_key("v1.2.3"));
        assert!(version_key("v20.0.0") > version_key("v9.9.9"));
        assert_eq!(version_key("24.18.0"), (24, 18, 0));
        assert_eq!(version_key("v24"), (24, 0, 0));
    }

    #[test]
    fn child_path_puts_node_and_cli_first_and_keeps_tool_dirs() {
        let cli = Path::new("/opt/homebrew/bin/dsh");
        let node = Path::new("/opt/homebrew/bin/node");
        let joined = child_path(cli, Some(node));
        let dirs: Vec<std::path::PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(
            dirs.first().map(|d| d.to_string_lossy().to_string()),
            Some("/opt/homebrew/bin".into())
        );
        // 后面还要跟着常见工具目录（pnpm 的几种装法都在那里）
        assert!(dirs
            .iter()
            .any(|d| d.to_string_lossy().ends_with(".local/share/pnpm")));
    }

    // ---- 孤儿回收：只认「我们的 profile + 已经是孤儿（ppid=1）+ 本用户」 ----

    #[test]
    fn orphan_instances_only_takes_our_own_orphans() {
        let ps = "\
           1       0     1     0 /sbin/launchd\n\
         501       1   501   501 node /usr/local/lib/node_modules/@deepseek-ai/dsh/lib/bin.js --profile wallpallperem --no-open --port 53233\n\
         502       1   502   501 node /usr/local/lib/node_modules/@deepseek-ai/dsh/lib/bin.js --profile web --no-open\n\
         503    4242   503   501 node /usr/local/lib/node_modules/@deepseek-ai/dsh/lib/bin.js --profile wallpallperem --no-open\n\
         504       1   504   502 node /usr/local/lib/node_modules/@deepseek-ai/dsh/lib/bin.js --profile wallpallperem --no-open\n\
         505       1   999   501 node /usr/local/lib/node_modules/@deepseek-ai/dsh/lib/bin.js plugin --profile wallpallperem add foo\n\
         506       1   506   501 node /usr/local/lib/node_modules/other-tool/lib/bin.js --profile wallpallperem\n";
        let got = orphan_instances(ps, 501);
        // 501：我们的 + 孤儿 + 本用户 + 自己是组长 → 整组收
        // 505：装插件那次（`dsh plugin`）同样是本应用起的，也回收；但它不是组长，
        //      只杀进程本身（别去动它继承来的、可能是用户前台组的那个组）
        let ids: Vec<i32> = got.iter().map(|l| l.pid).collect();
        assert_eq!(ids, vec![501, 505]);
        assert!(got[0].own_group);
        assert!(!got[1].own_group);
    }

    #[test]
    fn is_our_dsh_needs_both_marks() {
        assert!(is_our_dsh(
            "node /x/@deepseek-ai/dsh/lib/bin.js --profile wallpallperem --no-open --port 1"
        ));
        assert!(is_our_dsh("dsh plugin --profile wallpallperem add pkg"));
        // 别人 profilename 前缀撞车不算（整 token 比对）
        assert!(!is_our_dsh("node /x/dsh/bin.js --profile wallpallperemX"));
        // 用户自己的 dsh（别的 profile）不关我们的事
        assert!(!is_our_dsh("dsh web --no-open"));
        assert!(!is_our_dsh("node /x/other-tool --profile wallpallperem"));
    }

    // ---- 收尾：直接子进程与它生的子进程一起走 ----

    #[cfg(unix)]
    fn alive(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// 等进程真的从进程表消失（被杀的进程可能有极短的僵尸期）
    #[cfg(unix)]
    fn wait_dead(pid: i32) -> bool {
        for _ in 0..50 {
            if !alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// 有界等待自己的子进程退出并 reap（僵尸状态 kill(pid, 0) 仍然是成功，
    /// 必须靠 reap 确认它真的走完了收尾）
    #[cfg(unix)]
    fn wait_child_dead(child: &mut Child) -> bool {
        for _ in 0..50 {
            match child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => return false,
            }
        }
        false
    }

    /// 写一个「假 dsh」脚本：先起一个子进程（模拟 dsh 跑命令 / 装包时生的 node），
    /// 把两个 pid 写进目录，然后自己挂住不走。返回脚本路径。
    #[cfg(unix)]
    fn fake_dsh(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let script = dir.join(format!("{name}.sh"));
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nsleep 300 &\necho $! > '{g}'\necho $$ > '{c}'\nsleep 300\n",
                g = dir.join(format!("{name}-child.pid")).display(),
                c = dir.join(format!("{name}.pid")).display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[cfg(unix)]
    fn read_pid(path: &Path) -> Option<i32> {
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    /// 等「假 dsh」把两个 pid 都写出来：返回 (自己的 pid, 子进程的 pid)
    #[cfg(unix)]
    fn wait_pids(dir: &Path, name: &str) -> (i32, i32) {
        for _ in 0..200 {
            match (
                read_pid(&dir.join(format!("{name}.pid"))),
                read_pid(&dir.join(format!("{name}-child.pid"))),
            ) {
                (Some(a), Some(b)) => return (a, b),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        panic!("假 dsh 没起来");
    }

    #[cfg(unix)]
    fn spawn_fake(script: &Path) -> Child {
        let mut cmd = Command::new(script);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        in_own_group(&mut cmd);
        cmd.spawn().unwrap()
    }

    /// 关窗 / 停止 / 退出的收尾：dsh 自己 spawn 出来的子进程也要一起走 ——
    /// 只杀直接子进程会在用户桌面上留一串看不见的 node
    #[cfg(unix)]
    #[test]
    fn kill_tree_takes_grandchildren_too() {
        let dir = std::env::temp_dir().join(format!("we-dsh-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let script = fake_dsh(&dir, "tree");
        let mut child = spawn_fake(&script);
        let (me, kid) = wait_pids(&dir, "tree");
        assert_eq!(me as u32, child.id());
        assert!(alive(me) && alive(kid));

        kill_tree(&mut child);
        assert!(wait_dead(me), "直接子进程");
        assert!(wait_dead(kid), "子进程（只杀直接子进程的话就是它活下来）");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 启动中（还没等到就绪地址）就被收尾：进程已经在跑，这一段最容易漏
    #[cfg(unix)]
    #[test]
    fn kill_host_reaps_a_starting_child_and_cancels_the_start() {
        let dir = std::env::temp_dir().join(format!("we-dsh-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let script = fake_dsh(&dir, "start");
        let mut child = spawn_fake(&script);
        let (me, kid) = wait_pids(&dir, "start");

        // 模拟「启动流程已登记 pid、正在等就绪地址」
        let host = DshHost::default();
        *host.slot.lock().unwrap() = Slot::Starting { pid: me as u32 };
        assert!(matches!(host.snapshot(), Snapshot::Starting));
        let epoch = host.epoch.load(Ordering::SeqCst);

        host.kill(); // 关窗 / 退出
        assert!(
            host.epoch.load(Ordering::SeqCst) != epoch,
            "收尾必须让进行中的启动作废（epoch 要变）"
        );
        assert!(matches!(host.snapshot(), Snapshot::Idle));
        assert!(wait_child_dead(&mut child), "启动中的直接子进程");
        assert!(wait_dead(kid), "启动中的子进程");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 探测超时不能留下「永远挂在读管道上的线程 + 一个 node」
    #[cfg(unix)]
    #[test]
    fn probe_timeout_reaps_process_and_thread() {
        let dir = std::env::temp_dir().join(format!("we-dsh-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let script = fake_dsh(&dir, "probe");

        let started = Instant::now();
        // 脚本由探测自己拉起来（就是被测的那条路）：它会写出两个 pid，然后挂住
        let v = probe_version_within(&script, None, Duration::from_millis(800));
        assert!(v.is_none(), "一个不回答 --version 的 dsh：拿不到版本号");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "超时后应当立刻收场，而不是继续等（实测 {:?}）",
            started.elapsed()
        );
        let (me, kid) = wait_pids(&dir, "probe");
        assert!(wait_dead(me), "被探测的进程");
        assert!(wait_dead(kid), "它生的子进程");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 强杀 / 崩溃 / 开发重建留下的实例：启动时按「ppid=1 + 我们的 profile + 本用户」
    /// 捞出来收掉 —— 但**不能碰**还挂在父进程下的实例（那是用户自己在终端里开的）。
    #[cfg(unix)]
    #[test]
    fn reap_orphans_takes_the_leftover_but_not_a_parented_one() {
        let dir = std::env::temp_dir().join(format!("we-dsh-orphan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // 忽略 SIGTERM：顺带把「宽限期内不退就 SIGKILL」那段也走到
        let script = dir.join("dsh");
        std::fs::create_dir_all(&dir).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(
                &script,
                "#!/bin/sh\ntrap '' TERM\necho $$ > \"$(dirname \"$0\")/orphan.pid\"\nsleep 300\n",
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // 拉到 ppid=1：起一个 sh 把脚本放后台，然后 sh 自己退出 → 脚本被 launchd 收养
        let launched = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "'{}' web --profile {PROFILE} >/dev/null 2>&1 &",
                script.display()
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut launched = launched;
        let _ = launched.wait();
        let orphan = loop {
            if let Some(pid) = read_pid(&dir.join("orphan.pid")) {
                break pid;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // 对照组：同一个脚本，但还挂在测试进程下（ppid 不是 1）—— 绝不能碰
        let mut parented = spawn_fake(&script);
        let parented_pid = parented.id() as i32;
        assert!(
            ppid_of(orphan) == Some(1),
            "假 dsh 应当已成孤儿（ppid=1），实际 {:?}",
            ppid_of(orphan)
        );

        reap_orphans();

        assert!(
            wait_dead(orphan),
            "强杀遗留的实例应当被回收（它忽略 SIGTERM，靠宽限期后的 SIGKILL）"
        );
        assert!(
            alive(parented_pid),
            "还挂在父进程下的实例不归我们管，不能误杀"
        );
        kill_tree(&mut parented);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真实 `ps` 读取：拿一个 ppid=1 的假 dsh 验证整条探测链（口径 + 命令行匹配）
    #[cfg(unix)]
    fn ppid_of(pid: i32) -> Option<i32> {
        let out = Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }

    #[test]
    fn manifest_matches_dsh_init_profile_shape() {
        let m = profile_manifest();
        let v: Value = serde_json::from_str(&m).expect("valid json");
        assert_eq!(v["name"], json!("dsh-profile-wallpallperem"));
        assert_eq!(v["private"], json!(true));
        assert_eq!(v["dsh"]["profile"]["bundles"], json!(CLEAN_BUNDLES));
    }

    #[test]
    fn mcp_patch_body_is_parseable_and_points_at_the_url() {
        let body = mcp_patch_body("http://127.0.0.1:7411/mcp?token=abc");
        // 只做结构断言：YAML 里这一层是「一条 insert，插一条 mcp-client」
        assert!(body.contains("managed-by: wallpaperem-plugin"));
        assert!(body.contains("- insert:"));
        assert!(body.contains("id: wallpaperem-mcp"));
        assert!(body.contains("name: '@deepseek-ai/dsh-mcp-client'"));
        assert!(body.contains("serverName: wallpaperem"));
        assert!(body.contains("url: 'http://127.0.0.1:7411/mcp?token=abc'"));
        assert!(body.ends_with('\n'));
    }

    #[test]
    fn ensure_profile_creates_clean_files_and_rewrites_our_patch() {
        let root = std::env::temp_dir().join(format!("we-plugin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("profiles").join(PROFILE);

        let plan = ensure_profile(&dir, "http://127.0.0.1:1/mcp?token=a").unwrap();
        assert!(
            plan.overlay.is_none(),
            "clean profile: patch goes to cordis.patch.yml"
        );
        assert!(profile_is_clean(&dir));
        let patch = std::fs::read_to_string(dir.join(PATCH_FILE)).unwrap();
        assert!(patch.contains("token=a"));
        assert!(mcp_layer_current(&dir, "http://127.0.0.1:1/mcp?token=a"));

        // 换端口/令牌（服务重启后会变）：同一份 patch 原地重写，不堆叠
        ensure_profile(&dir, "http://127.0.0.1:2/mcp?token=b").unwrap();
        let patch = std::fs::read_to_string(dir.join(PATCH_FILE)).unwrap();
        assert!(patch.contains("token=b") && !patch.contains("token=a"));
        assert_eq!(patch.matches("- insert:").count(), 1);

        // 用户手改过 patch：不再覆盖它，改用独立 overlay
        std::fs::write(dir.join(PATCH_FILE), "- id: mine\n  config: {}\n").unwrap();
        let plan = ensure_profile(&dir, "http://127.0.0.1:3/mcp?token=c").unwrap();
        assert_eq!(
            plan.overlay.as_deref(),
            Some(dir.join(OVERLAY_FILE).as_path())
        );
        assert!(std::fs::read_to_string(dir.join(PATCH_FILE))
            .unwrap()
            .contains("mine"));
        assert!(std::fs::read_to_string(dir.join(OVERLAY_FILE))
            .unwrap()
            .contains("token=c"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_manifest_is_never_touched() {
        let root = std::env::temp_dir().join(format!("we-plugin-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("profiles").join(PROFILE);
        std::fs::create_dir_all(&dir).unwrap();
        let mine = "{\n  \"name\": \"custom\"\n}\n";
        std::fs::write(dir.join("package.json"), mine).unwrap();
        ensure_profile(&dir, "http://127.0.0.1:1/mcp?token=a").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("package.json")).unwrap(),
            mine
        );
        assert!(!profile_is_clean(&dir));
        let _ = std::fs::remove_dir_all(&root);
    }
}
