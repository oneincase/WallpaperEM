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
use std::sync::atomic::{AtomicBool, Ordering};
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
/// 往 profile 装插件包（pnpm + 联网）：dsh 自己就会等最多 2 分钟的写锁/查找，
/// 这里给到 5 分钟
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
/// 诊断输出只留最后这么多行
const MAX_TAIL_LINES: usize = 40;
/// 全局装一个 pnpm（npm + 联网）：给 3 分钟够了
const PNPM_INSTALL_TIMEOUT: Duration = Duration::from_secs(180);

// ---------------------------------------------------------------- 错误

// ---------------------------------------------------------------- 状态

/// 插件宿主状态：当前只有 dsh 子进程需要持有。
#[derive(Default)]
pub struct DshHost {
    proc: Mutex<Option<Running>>,
    /// 正在启动：防连点造成两个 dsh 进程（后一个会覆盖句柄，前一个变孤儿）
    starting: AtomicBool,
}

struct Running {
    child: Child,
    url: String,
}

/// 起启动互斥：无论成功失败都要把 starting 放回去（`?` 提前返回时靠 Drop）。
struct StartGuard<'a>(&'a AtomicBool);

impl Drop for StartGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

pub fn init(app: &AppHandle) {
    app.manage(DshHost::default());
}

/// 窗口销毁 / 应用退出：把子进程收掉，别在用户桌面上留看不见的 node。
pub fn shutdown(app: &AppHandle) {
    if let Some(host) = app.try_state::<DshHost>() {
        kill(&host);
    }
}

fn kill(host: &DshHost) {
    if let Ok(mut slot) = host.proc.lock() {
        if let Some(mut r) = slot.take() {
            let _ = r.child.kill();
            let _ = r.child.wait();
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
    cmd
}

/// `dsh --version`。探测超时（10s）就放弃版本号，不阻塞扫描。
fn probe_version(cli: &Path, node: Option<&Path>) -> Option<String> {
    let cli = cli.to_path_buf();
    let node = node.map(|n| n.to_path_buf());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let out = cli_command(&cli, node.as_deref()).arg("--version").output();
        let _ = tx.send(out);
    });
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(o)) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                None
            } else {
                // 多行输出只取第一行（版本号那行）
                Some(s.lines().next().unwrap_or("").trim().to_string())
            }
        }
        _ => None,
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

/// 在跑的实例还活着吗？活着返回它的 URL；进程已退出则清掉句柄。
fn live_url(host: &DshHost) -> Option<String> {
    let mut slot = host.proc.lock().ok()?;
    if let Some(r) = slot.as_mut() {
        match r.child.try_wait() {
            Ok(None) => return Some(r.url.clone()),
            _ => {
                let mut dead = slot.take().unwrap();
                let _ = dead.child.wait();
            }
        }
    }
    None
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
fn wait_for_url(child: &mut Child, port: u16) -> Result<String, PluginError> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| PluginError::code("dsh-no-stdout"))?;
    let stderr = child.stderr.take();
    let (tx, rx): (_, Receiver<String>) = mpsc::channel();

    std::thread::spawn(move || {
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
        std::thread::spawn(move || {
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
    let _ = child.kill();
    let _ = child.wait();
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

    if let Some(host) = app.try_state::<DshHost>() {
        if let Some(url) = live_url(&host) {
            return Ok(Started { url, port: 0 });
        }
    }

    let port = free_port()?;
    let mut child = spawn_dsh(&cli, node.as_deref(), plan.overlay.as_deref(), port, &dir)?;
    match wait_for_url(&mut child, port) {
        Ok(url) => {
            if let Some(host) = app.try_state::<DshHost>() {
                // 放进状态前先清掉可能残留的旧句柄，否则又漏一个进程
                kill(&host);
                if let Ok(mut slot) = host.proc.lock() {
                    *slot = Some(Running {
                        child,
                        url: url.clone(),
                    });
                }
            }
            Ok(Started { url, port })
        }
        Err(e) => Err(e),
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
        .and_then(|h| live_url(&h))
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
    if host.starting.swap(true, Ordering::SeqCst) {
        return Err(PluginError::code("dsh-starting"));
    }
    let _guard = StartGuard(&host.starting);
    ensure_running(&app).await
}

/// 打开（或聚焦）dsh 窗口：装好 MCP / 干净 profile，启动 dsh，再把它的页面开成窗口。
///
/// 与 `dsh_open` 的分工：这里只做「把它弄起来」，连点保护与状态判断在外面 ——
/// 第三方 `dsh-profile` 插件（往 profile 装包的那种）也走这条路径。
pub(crate) async fn ensure_running(app: &AppHandle) -> Result<Value, PluginError> {
    let host = app.state::<DshHost>();
    let alive = live_url(&host);
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
        handles.push(drain(pipe, "dsh", out_buf.clone()));
    }
    if let Some(pipe) = child.stderr.take() {
        handles.push(drain(pipe, "dsh", err_buf.clone()));
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
            let _ = child.kill();
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

/// 把一条管道读到 EOF，只留最后 [`MAX_TAIL_LINES`] 行。
fn drain<R: std::io::Read + Send + 'static>(
    pipe: R,
    tag: &'static str,
    buf: Arc<Mutex<VecDeque<String>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
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
}

/// 停掉后台的 dsh 进程（窗口由用户自己关；这里给「停止」按钮用）。
#[tauri::command]
pub fn dsh_stop(app: AppHandle) -> Value {
    if let Some(host) = app.try_state::<DshHost>() {
        kill(&host);
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
