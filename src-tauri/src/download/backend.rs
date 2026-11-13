//! 下载后端：命令行拼装、输出解析、产物布局。
//!
//! 队列、状态机、产物收编、依赖补拉由 `download/mod.rs` 承担，本模块只管
//! 「怎么跟下载工具这个子进程打交道」，并在两个实现之间分发：
//!
//! - [`SteamCmd`]（`self`，Valve 官方，默认）：三个非显然的约束（都是踩过的坑）——
//!   **stdin 必须是 TTY**（管道会让它直接打印 "cannot read from the console" 退出，
//!   见 `pty.rs`）、**下载成功后常不退出**（macOS 上 Steam API 拆卸有线程竞争，
//!   `+quit` 无响应，必须靠 `match_success` 匹配到成功行后主动杀进程，不能等退出码）、
//!   **下载期间零进度输出**（`workshop_download_item` 只在首尾各打一行，进度由
//!   mod.rs 轮询产物目录体积估算）
//! - [`super::depotdl::DepotDl`]（第三方，可选）：细节见该模块头注释
//!
//! [`Backend`] 是二者的统一门面，`download_backend` 设置决定实例化哪一个。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::download::depotdl::DepotDl;
use crate::download::APP_ID;

/// 登录凭据。`has_token` 为真时表示 steamcmd 已缓存登录态，无需再传密码。
pub struct Credentials {
    pub username: String,
    pub password: String,
    pub has_token: bool,
}

/// 子进程正在索要的交互输入类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardKind {
    /// 邮箱验证码 / 手机令牌验证码，需要用户输入
    Code,
    /// 密码提示（登录态失效时 steamcmd 会重新索要密码）
    Password,
    /// 等待手机 App 点确认，无需输入，继续等待即可
    MobileConfirm,
}

/// 成功行：`Success. Downloaded item 12345 to "/path" (123 bytes)`
const SUCCESS_RE: &str = r"(?i)Success\.\s*Downloaded item\s+\d+";
/// 登录成功行（仅登录验证任务用）：Logging in user 'x' to Steam Public...OK
const LOGIN_SUCCESS_RE: &str = r"(?i)Logging in user .+ to Steam Public\.\.\.OK";

/// 登录验证任务专用的 item_id（不对应任何工坊条目，前端按此显示「账号登录验证」）
pub const VERIFY_ITEM_ID: &str = "verify:login";
/// 验证码提示（均不带换行，靠字节级读取器捕获）
const CODE_RE: &str =
    r"(?i)steam ?guard code\s*:|two-factor code\s*:|enter the current code from your steam guard";
const PASSWORD_RE: &str = r"(?i)^password\s*:|\bpassword\s*:\s*$";
const MOBILE_RE: &str =
    r"(?i)confirm (the|this) login|steam mobile app|waiting for confirmation|please approve";

/// 密码登录开始的标记。
///
/// 新版 steamcmd（实测 client 1788292693）在等待手机 App 确认时**不打印任何提示**，
/// 控制台日志里只有 "Logging in user ..." 之后长达几十秒的静默（OK 出现的时间差
/// 就是用户在手机上确认的时间）。因此「等待手机确认」不能靠匹配确认文案，只能靠
/// 「密码登录已开始 + 长时间未完成」推测——本正则就是前半段的探测器。
///
/// 两个信号（console_log.txt 实测）：
/// - "Logging in using username/password."（密码登录才会打印；缓存登录是另一句）
/// - "Logging in user 'x' [U:1:0] to Steam Public..."（密码登录时 steamid 恒为 U:1:0，
///   缓存登录会带上真实 steamid 如 [U:1:351868315]）
const LOGIN_START_RE: &str = r"(?i)logging in using username/password|logging in user .*\[u:1:0\]";

/// 工坊大件常见，且 steamcmd 下载期间完全静默，看门狗必须给足时间
pub const WATCHDOG: std::time::Duration = std::time::Duration::from_secs(30 * 60);

#[derive(Clone)]
pub struct SteamCmd {
    /// `<app_data>/steamcmd/steamcmd.sh`
    pub script: PathBuf,
    /// 重定向给子进程的 HOME，隔离登录态、避免污染用户真实 Steam 配置
    pub home: PathBuf,
}

impl SteamCmd {
    pub fn is_installed(&self) -> bool {
        self.script.exists()
    }

    /// 下载单个工坊条目的命令行参数（不含程序自身）。
    pub fn build_args(&self, item_id: &str, workdir: &Path, cred: &Credentials) -> Vec<OsString> {
        // 注意：force_install_dir 必须在 login 之前且为绝对路径，
        // 否则 steamcmd 静默把内容下到 $HOME/Steam（其二进制会打印
        // "Please use force_install_dir before logon!"）。
        let mut args: Vec<OsString> = vec![
            "+@ShutdownOnFailedCommand".into(),
            "1".into(),
            "+force_install_dir".into(),
            workdir.into(),
            "+login".into(),
            cred.username.as_str().into(),
        ];
        if !cred.has_token {
            args.push(cred.password.as_str().into());
        }
        args.push("+workshop_download_item".into());
        args.push(APP_ID.into());
        args.push(item_id.into());
        args.push("+quit".into());
        args
    }

    /// 仅登录验证的命令行参数（不下载任何内容，登录成功后直接退出）
    pub fn build_login_args(&self, workdir: &Path, cred: &Credentials) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "+@ShutdownOnFailedCommand".into(),
            "1".into(),
            "+force_install_dir".into(),
            workdir.into(),
            "+login".into(),
            cred.username.as_str().into(),
        ];
        if !cred.has_token {
            args.push(cred.password.as_str().into());
        }
        args.push("+quit".into());
        args
    }

    /// 该行输出是否表示登录成功（仅登录验证任务用）
    pub fn match_login_success(&self, s: &str) -> bool {
        re(LOGIN_SUCCESS_RE).is_match(s)
    }

    /// 该行（或无换行的提示尾部）是否表示一次密码登录刚刚开始。
    /// 见 LOGIN_START_RE 的注释：用于推测后续可能进入「静默等待手机确认」阶段。
    pub fn match_login_start(&self, s: &str) -> bool {
        re(LOGIN_START_RE).is_match(s)
    }

    /// 需要注入的环境变量。
    ///
    /// steamcmd 没有 configdir 参数，登录态固定写在用户主目录下
    /// （macOS: `~/Library/Application Support/Steam/config/config.vdf`；
    /// Windows: `%USERPROFILE%\AppData\Local\Steam`）。
    /// 覆盖对应的家目录变量即可把它关进应用自己的目录。
    pub fn extra_env(&self) -> Vec<(String, OsString)> {
        vec![(
            crate::download::steamcmd_install::home_env_key().to_string(),
            self.home.as_os_str().into(),
        )]
    }

    /// 产物落地目录（相对 workdir）。
    pub fn artifact_dir(&self, workdir: &Path, item_id: &str) -> PathBuf {
        workdir
            .join("steamapps")
            .join("workshop")
            .join("content")
            .join(APP_ID)
            .join(item_id)
    }

    /// 进度统计目录：steamcmd 下载期间先写 `workshop/downloads/`，全部完成才
    /// 搬到 `workshop/content/` —— 只看 content 会在整个下载期恒为 0，
    /// 两处合计才是真实的已下字节数。
    pub fn progress_dirs(&self, workdir: &Path, item_id: &str) -> Vec<PathBuf> {
        let base = workdir.join("steamapps").join("workshop");
        vec![
            base.join("downloads").join(APP_ID).join(item_id),
            base.join("content").join(APP_ID).join(item_id),
        ]
    }

    /// 该行输出是否在索要交互输入。
    pub fn match_guard_prompt(&self, s: &str) -> Option<GuardKind> {
        if re(CODE_RE).is_match(s) {
            return Some(GuardKind::Code);
        }
        if re(MOBILE_RE).is_match(s) {
            return Some(GuardKind::MobileConfirm);
        }
        if re(PASSWORD_RE).is_match(s) {
            return Some(GuardKind::Password);
        }
        None
    }

    /// 该行输出是否表示下载成功。
    pub fn match_success(&self, s: &str) -> bool {
        re(SUCCESS_RE).is_match(s)
    }

    /// 该行输出是否表示失败，返回面向用户的中文原因。
    pub fn parse_failure(&self, s: &str) -> Option<String> {
        let l = s.to_ascii_lowercase();
        // steamcmd 把「账号未拥有该游戏」和「真的连不上」都报成 No Connection，
        // 无法区分，只能把两种可能都告诉用户。
        if l.contains("failed (no connection)") {
            return Some(
                "下载失败（steamcmd 报 No Connection）。可能是网络不通，也可能是该账号未拥有 \
                 Wallpaper Engine —— steamcmd 对这两种情况返回同一个错误。请到「设置 → 网络」\
                 做一次 Steam 连通性探测来区分。"
                    .into(),
            );
        }
        if l.contains("failed (no match)") {
            return Some("该工坊条目不存在或已被作者删除".into());
        }
        if l.contains("failed (access denied)") {
            return Some("下载被拒绝：该账号无权访问此条目".into());
        }
        if l.contains("failed (file not found)") {
            return Some("Steam 未找到该条目的文件".into());
        }
        if l.contains("timeout downloading item") {
            return Some("下载超时，请重试或检查网络/代理".into());
        }
        if l.contains("failed to start downloading item") {
            return Some("无法开始下载，通常是登录态失效，请重新登录".into());
        }
        if l.contains("no cached credentials and @nopromptforpassword is set") {
            return Some("登录态已失效，请到「设置 → 账号」重新登录".into());
        }
        if l.contains("cached credentials not found") {
            return Some("未找到登录态，请先到「设置 → 账号」登录".into());
        }
        if l.contains("cannot read from the console") {
            return Some("steamcmd 无法读取终端输入（PTY 分配失败），请重试".into());
        }
        if l.contains("rate limit") || l.contains("too many login attempts") {
            return Some("登录过于频繁，请稍后再试".into());
        }
        if l.contains("invalid password") || l.contains("password incorrect") {
            return Some("账号或密码错误".into());
        }
        if l.contains("account logon denied") || l.contains("login failure") {
            return Some(format!(
                "Steam 登录被拒绝：{}",
                s.chars().take(120).collect::<String>()
            ));
        }
        if l.contains("two-factor code mismatch") || l.contains("steam guard code was invalid") {
            return Some("验证码错误，请重新输入".into());
        }
        if l.contains("account has been locked") {
            return Some(format!(
                "Steam 账号已被锁定：{}",
                s.chars().take(120).collect::<String>()
            ));
        }
        if l.contains("no license") || l.contains("no subscription") || l.contains("not licensed") {
            return Some("该账号未拥有 Wallpaper Engine".into());
        }
        if l.contains("multiple logins") {
            return Some("该账号在其他设备登录，请稍后再试".into());
        }
        if l.contains("unable to connect") || l.contains("failed to connect") {
            return Some(format!(
                "无法连接 Steam 服务器：{}",
                s.chars().take(120).collect::<String>()
            ));
        }
        None
    }

    /// 该行是否是「登录过于频繁」（Steam 限流）。
    /// 这类失败**不能自动重试**：队列几条任务同时重登只会把限流窗口越拖越长。
    pub fn is_rate_limit(&self, s: &str) -> bool {
        let l = s.to_ascii_lowercase();
        l.contains("rate limit") || l.contains("too many login attempts")
    }
}

/// 无需构造实例的提示判定，供字节级读取器的 'static 闭包使用。
pub fn is_prompt(tail: &str) -> bool {
    // "Logging in user..." 也算提示：steamcmd 打印该句后不带换行地阻塞等待，
    // 不把这类尾部文本抛出来，就永远探测不到「密码登录已开始」（LOGIN_START_RE
    // 的第二个信号 [U:1:0] 正藏在这行里）。
    re(CODE_RE).is_match(tail)
        || re(MOBILE_RE).is_match(tail)
        || re(PASSWORD_RE).is_match(tail)
        || re(LOGIN_START_RE).is_match(tail)
}

/// 编译并缓存正则。按模式缓存，避免每行输出现场编译。
/// 两个后端共用（各自的模式是各自的编译期常量）。
pub(crate) fn cached_regex(pattern: &'static str) -> &'static regex::Regex {
    use std::collections::HashMap;
    use std::sync::{OnceLock, RwLock};

    static CACHE: OnceLock<RwLock<HashMap<&'static str, &'static regex::Regex>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    if let Some(r) = cache.read().ok().and_then(|c| c.get(pattern).copied()) {
        return r;
    }
    // 模式均为编译期常量，编译失败属于开发期错误。
    // 退化为一个永不匹配的模式（`$a` —— 行尾之后还要有字符，不可能成立），
    // 注意不能用 `(?!)`：Rust 的 regex 不支持 look-around，那样会连 fallback 一起 panic。
    let compiled: &'static regex::Regex =
        Box::leak(Box::new(regex::Regex::new(pattern).unwrap_or_else(|e| {
            tracing::error!("下载后端正则编译失败（{pattern}）: {e}");
            regex::Regex::new(r"$a").expect("fallback regex 必须可编译")
        })));
    if let Ok(mut c) = cache.write() {
        c.insert(pattern, compiled);
    }
    compiled
}

fn re(pattern: &'static str) -> &'static regex::Regex {
    cached_regex(pattern)
}

/// 递归统计目录内的字节数（steamcmd 无进度输出时用它估算下载进度）。
pub fn dir_size(path: &Path) -> u64 {
    dir_size_excluding(path, &[])
}

/// 产物目录里最新的文件修改时间（递归，跳过顶层 `skip` 目录名）。
///
/// steamcmd 会**预分配整个文件**（实测 20MB 的 scene.pkg 在下载开始 7 秒后
/// 就已经是最终大小 20,067,115B，之后几分钟里边写边大小不变），所以「停滞」
/// 只能靠写入活动（mtime）判定，不能靠体积增长 —— 按体积判会把还在写的大文件
/// 当成已下齐，收编出「长度对、中间全零」的坏文件。
pub fn latest_mtime(path: &Path, skip: &[&str]) -> Option<std::time::SystemTime> {
    fn walk(dir: &Path, skip: &[&str], acc: &mut Option<std::time::SystemTime>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            if skip.iter().any(|s| name == std::ffi::OsStr::new(s)) {
                continue;
            }
            match e.metadata() {
                Ok(m) if m.is_dir() => walk(&e.path(), skip, acc),
                Ok(m) => {
                    if let Ok(t) = m.modified() {
                        if acc.map(|cur| t > cur).unwrap_or(true) {
                            *acc = Some(t);
                        }
                    }
                }
                Err(_) => {}
            }
        }
    }
    let mut acc = None;
    if path.is_dir() {
        walk(path, skip, &mut acc);
    }
    acc
}

/// 目录内是否存在 ≥ `threshold` 字节的连续零段。
///
/// 用途：收编停滞产物前的完整性兜底。steamcmd 预分配后，没写完的区域就是
/// 整片 0 —— 被提前收尾的坏 pkg 实测有 1MB/2MB/3MB 的页对齐零段；而本机
/// 70+ 个正常 pkg 的最大零段 ≤ 762KB（压缩容器里极少出现长零段）。
/// 命中即视为「预分配未写完」，不能当成功收编。
pub fn has_zero_hole(path: &Path, threshold: u64) -> bool {
    const CHUNK: usize = 1 << 20;
    let mut buf = vec![0u8; CHUNK];
    let mut run: u64 = 0;

    fn scan_file(f: &Path, buf: &mut [u8], run: &mut u64, threshold: u64) -> bool {
        let Ok(mut file) = std::fs::File::open(f) else {
            return false;
        };
        use std::io::Read;
        loop {
            let Ok(n) = file.read(buf) else {
                return false;
            };
            if n == 0 {
                return false;
            }
            for &b in &buf[..n] {
                if b == 0 {
                    *run += 1;
                    if *run >= threshold {
                        return true;
                    }
                } else {
                    *run = 0;
                }
            }
        }
    }

    fn walk(dir: &Path, buf: &mut [u8], run: &mut u64, threshold: u64) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for e in entries.flatten() {
            let p = e.path();
            match e.metadata() {
                Ok(m) if m.is_dir() => {
                    if walk(&p, buf, run, threshold) {
                        return true;
                    }
                }
                Ok(m) if m.is_file() => {
                    if m.len() < threshold {
                        continue;
                    }
                    // 分段之间要连续计数：不清 run（跨文件不连续，但每文件开头清零）
                    *run = 0;
                    if scan_file(&p, buf, run, threshold) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    if !path.is_dir() {
        return false;
    }
    walk(path, &mut buf, &mut run, threshold)
}

/// 收尾兜底：把 steamcmd 僵在 `downloads/` 里的成品搬进 `content/`。
/// 触发场景见 `mod.rs` 的 `STALL_COMMIT_GRACE` —— 字节已下齐但提交失败，
/// 成品永远不会自己挪过去。同一 workdir 内 rename，不跨卷。
/// 返回搬移的条目数；`downloads` 不存在时返回 0（视为无产物）。
pub fn commit_stalled_artifact(downloads: &Path, content: &Path) -> std::io::Result<u64> {
    if !downloads.is_dir() {
        return Ok(0);
    }
    std::fs::create_dir_all(content)?;
    let mut moved = 0u64;
    for entry in std::fs::read_dir(downloads)?.flatten() {
        let to = content.join(entry.file_name());
        std::fs::rename(entry.path(), &to)?;
        moved += 1;
    }
    Ok(moved)
}

/// 同 [`dir_size`]，但跳过 `skip` 里的顶层目录名（后端自己的元数据目录）。
/// DepotDownloader 的 `.DepotDownloader/` 就落在产物根下，不排除的话
/// 「只下到元数据」也会被当成有产物。
pub fn dir_size_excluding(path: &Path, skip: &[&str]) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0u64;
    for e in entries.flatten() {
        let name = e.file_name();
        if skip.iter().any(|s| name == std::ffi::OsStr::new(s)) {
            continue;
        }
        match e.file_type() {
            Ok(t) if t.is_dir() => total += dir_size(&e.path()),
            Ok(t) if t.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => {}
        }
    }
    total
}

// ---------- 后端选择 ----------

/// 下载工具种类（设置键 `download_backend` 的取值）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// Valve 官方 steamcmd（默认）
    SteamCmd,
    /// 第三方 DepotDownloader（需另行下载，约 33 MB）
    DepotDownloader,
}

impl BackendKind {
    /// 缺省与非法值都回落到 steamcmd：默认工具不因历史残留设置而改变
    pub fn from_setting(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some("depotdownloader") | Some("depot_downloader") | Some("DepotDownloader") => {
                Self::DepotDownloader
            }
            _ => Self::SteamCmd,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SteamCmd => "steamcmd",
            Self::DepotDownloader => "depotdownloader",
        }
    }

    /// 面向用户/错误信息的名字
    pub fn label(&self) -> &'static str {
        match self {
            Self::SteamCmd => "steamcmd",
            Self::DepotDownloader => "DepotDownloader",
        }
    }

    /// 该工具的登录态标记设置键。
    ///
    /// 必须按后端分开：两个工具各自缓存登录态（steamcmd 在重定向 HOME 的
    /// config.vdf，DD 在 .NET IsolatedStorage），换工具后原来的令牌用不上，
    /// 拿旧标记省略密码会让新工具卡在「请输入密码」上。
    pub fn token_setting(&self) -> &'static str {
        match self {
            Self::SteamCmd => "download_has_token",
            Self::DepotDownloader => "depotdl_has_token",
        }
    }

    /// 登录验证成功标记（下载页/设置页展示用；与后端一一对应）
    pub fn logged_in_setting(&self) -> &'static str {
        match self {
            Self::SteamCmd => crate::download::steamcmd_install::SETTING_LOGGED_IN,
            Self::DepotDownloader => crate::download::depotdl_install::SETTING_LOGGED_IN,
        }
    }

    /// 未安装时的失败错误码（前端用于区分「工具没装」这类可自愈失败）
    pub fn missing_code(&self) -> &'static str {
        match self {
            Self::SteamCmd => "STEAMCMD_NOT_FOUND",
            Self::DepotDownloader => "DEPOTDL_NOT_FOUND",
        }
    }
}

/// 当前选中的下载后端。所有「跟子进程打交道」的差异都在这里分发，
/// `download/mod.rs` 对两个工具一视同仁。
#[derive(Clone)]
pub enum Backend {
    SteamCmd(SteamCmd),
    DepotDownloader(DepotDl),
}

impl Backend {
    pub fn kind(&self) -> BackendKind {
        match self {
            Self::SteamCmd(_) => BackendKind::SteamCmd,
            Self::DepotDownloader(_) => BackendKind::DepotDownloader,
        }
    }

    pub fn label(&self) -> &'static str {
        self.kind().label()
    }

    /// 登录态标记的设置键（见 [`BackendKind::token_setting`]）
    pub fn token_setting(&self) -> &'static str {
        self.kind().token_setting()
    }

    pub fn program(&self) -> &Path {
        match self {
            Self::SteamCmd(b) => &b.script,
            Self::DepotDownloader(b) => &b.bin,
        }
    }

    pub fn is_installed(&self) -> bool {
        match self {
            Self::SteamCmd(b) => b.is_installed(),
            Self::DepotDownloader(b) => b.is_installed(),
        }
    }

    /// 未安装时的失败提示（指向设置页的对应入口）
    pub fn install_hint(&self) -> &'static str {
        match self {
            Self::SteamCmd(_) => "steamcmd 尚未安装，请到「设置 → 账号」点击安装",
            Self::DepotDownloader(_) => {
                "DepotDownloader 尚未安装，请到「设置 → 账号」切换下载工具后点击安装"
            }
        }
    }

    /// 需要注入的环境变量（两个工具都要重定向家目录隔离登录态）
    pub fn extra_env(&self) -> Vec<(String, OsString)> {
        match self {
            Self::SteamCmd(b) => b.extra_env(),
            Self::DepotDownloader(b) => b.extra_env(),
        }
    }

    /// 子进程工作目录（DD 需要：account.config 是相对 CWD 读写的）
    pub fn current_dir(&self) -> Option<PathBuf> {
        match self {
            Self::SteamCmd(_) => None,
            Self::DepotDownloader(b) => b.current_dir(),
        }
    }

    /// 单个条目的下载参数。`login_id` 只有 DD 用（并发实例要各自不同的 LogonID），
    /// steamcmd 忽略。
    pub fn build_args(
        &self,
        item_id: &str,
        workdir: &Path,
        cred: &Credentials,
        login_id: i64,
    ) -> Vec<OsString> {
        match self {
            Self::SteamCmd(b) => b.build_args(item_id, workdir, cred),
            Self::DepotDownloader(b) => b.build_args(item_id, workdir, cred, login_id),
        }
    }

    /// 登录验证任务的参数
    pub fn build_login_args(
        &self,
        workdir: &Path,
        cred: &Credentials,
        login_id: i64,
    ) -> Vec<OsString> {
        match self {
            Self::SteamCmd(b) => b.build_login_args(workdir, cred),
            Self::DepotDownloader(b) => b.build_login_args(workdir, cred, login_id),
        }
    }

    pub fn match_login_success(&self, s: &str) -> bool {
        match self {
            Self::SteamCmd(b) => b.match_login_success(s),
            Self::DepotDownloader(b) => b.match_login_success(s),
        }
    }

    /// 是否「密码登录已开始」（只有 steamcmd 需要靠它推测手机确认）
    pub fn match_login_start(&self, s: &str) -> bool {
        match self {
            Self::SteamCmd(b) => b.match_login_start(s),
            Self::DepotDownloader(_) => false,
        }
    }

    pub fn match_success(&self, s: &str) -> bool {
        match self {
            Self::SteamCmd(b) => b.match_success(s),
            Self::DepotDownloader(b) => b.match_success(s),
        }
    }

    pub fn match_guard_prompt(&self, s: &str) -> Option<GuardKind> {
        match self {
            Self::SteamCmd(b) => b.match_guard_prompt(s),
            Self::DepotDownloader(b) => b.match_guard_prompt(s),
        }
    }

    pub fn parse_failure(&self, s: &str) -> Option<String> {
        match self {
            Self::SteamCmd(b) => b.parse_failure(s),
            Self::DepotDownloader(b) => b.parse_failure(s),
        }
    }

    /// 该行是否是 Steam 的登录限流（不能自动重试，见各后端实现注释）
    pub fn is_rate_limit(&self, s: &str) -> bool {
        match self {
            Self::SteamCmd(b) => b.is_rate_limit(s),
            Self::DepotDownloader(b) => b.is_rate_limit(s),
        }
    }

    /// 进度是否来自输出行（DD 逐文件打百分比）而不是轮询目录体积（steamcmd）
    pub fn progress_from_output(&self) -> bool {
        matches!(self, Self::DepotDownloader(_))
    }

    /// 从输出行解析百分比（仅 [`Self::progress_from_output`] 为真的后端）
    pub fn parse_progress(&self, s: &str) -> Option<f64> {
        match self {
            Self::SteamCmd(_) => None,
            Self::DepotDownloader(b) => b.parse_progress(s),
        }
    }

    /// 收编前把输出行收拾成可展示的样子（DD 会混入 ANSI/OSC 控制序列）
    pub fn sanitize_line(&self, line: String) -> String {
        match self {
            Self::SteamCmd(_) => line,
            Self::DepotDownloader(_) => crate::download::depotdl::strip_ansi(&line),
        }
    }

    pub fn artifact_dir(&self, workdir: &Path, item_id: &str) -> PathBuf {
        match self {
            Self::SteamCmd(b) => b.artifact_dir(workdir, item_id),
            Self::DepotDownloader(b) => b.artifact_dir(workdir, item_id),
        }
    }

    /// 进度轮询要统计大小的目录（steamcmd 为 downloads+content 两处合计；
    /// DD 进度来自输出行，不走这里）
    pub fn progress_dirs(&self, workdir: &Path, item_id: &str) -> Vec<PathBuf> {
        match self {
            Self::SteamCmd(b) => b.progress_dirs(workdir, item_id),
            Self::DepotDownloader(b) => vec![b.artifact_dir(workdir, item_id)],
        }
    }

    /// 产物是否真实落地（成功判定用；两个后端的判据不同）
    pub fn artifact_ready(&self, workdir: &Path, item_id: &str) -> bool {
        match self {
            Self::SteamCmd(b) => {
                let dir = b.artifact_dir(workdir, item_id);
                dir.is_dir() && dir_size(&dir) > 0
            }
            Self::DepotDownloader(b) => b.artifact_ready(workdir),
        }
    }

    /// 收编时要跳过的元数据目录名（各处布局的元数据目录不同）
    pub fn is_metadata_entry(&self, name: &str) -> bool {
        match self {
            Self::SteamCmd(_) => name == "steamapps",
            Self::DepotDownloader(b) => b.is_metadata_entry(name),
        }
    }

    /// 退出码是否可信（steamcmd 下载成功后可能卡住不退出，见模块头注释）
    pub fn trust_exit_code(&self) -> bool {
        match self {
            Self::SteamCmd(_) => false,
            Self::DepotDownloader(b) => b.trust_exit_code(),
        }
    }

    /// 登录态是否**真的**还在（不只是我们写下的乐观标记）。
    ///
    /// - steamcmd：令牌写在被重定向 HOME 的 `config.vdf`（Steam 自己维护的格式），
    ///   检查成本高、历史上也没出过「标记与实况不符」→ 直接信标记
    /// - DepotDownloader：令牌是隔离目录里的一枚 `account.config`，一眼可查；
    ///   查不到就带上密码走密码登录，别让任务卡在「请输入密码」上
    pub fn has_stored_session(&self) -> bool {
        match self {
            Self::SteamCmd(_) => true,
            Self::DepotDownloader(b) => b.has_stored_session(),
        }
    }

    /// 子进程输出的「未换行尾部」是否是需要用户输入的提示
    pub fn prompt_detector(&self) -> fn(&str) -> bool {
        match self {
            Self::SteamCmd(_) => is_prompt,
            Self::DepotDownloader(_) => crate::download::depotdl::is_prompt,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sc() -> SteamCmd {
        SteamCmd {
            script: PathBuf::from("/tmp/steamcmd/steamcmd.sh"),
            home: PathBuf::from("/tmp/steamcmd-home"),
        }
    }

    #[test]
    fn args_put_install_dir_before_login() {
        let b = sc();
        let cred = Credentials {
            username: "alice".into(),
            password: "pw".into(),
            has_token: false,
        };
        let args: Vec<String> = b
            .build_args("123", Path::new("/work"), &cred)
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let dir_at = args.iter().position(|a| a == "+force_install_dir").unwrap();
        let login_at = args.iter().position(|a| a == "+login").unwrap();
        assert!(dir_at < login_at, "force_install_dir 必须在 login 之前");
        assert_eq!(args[dir_at + 1], "/work");
        assert_eq!(args[login_at + 1], "alice");
        assert_eq!(args[login_at + 2], "pw");
        assert!(args.ends_with(&["+quit".to_string()]));
    }

    #[test]
    fn args_omit_password_when_token_present() {
        let b = sc();
        let cred = Credentials {
            username: "alice".into(),
            password: String::new(),
            has_token: true,
        };
        let args: Vec<String> = b
            .build_args("123", Path::new("/work"), &cred)
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let login_at = args.iter().position(|a| a == "+login").unwrap();
        assert_eq!(args[login_at + 1], "alice");
        assert_eq!(args[login_at + 2], "+workshop_download_item");
    }

    #[test]
    fn artifact_dir_is_workshop_layout() {
        assert_eq!(
            sc().artifact_dir(Path::new("/work"), "999"),
            PathBuf::from("/work/steamapps/workshop/content/431960/999")
        );
    }

    #[test]
    fn progress_dirs_cover_downloads_and_content() {
        // steamcmd 下载期间先写 downloads/，进度统计必须两处合计
        assert_eq!(
            sc().progress_dirs(Path::new("/work"), "999"),
            vec![
                PathBuf::from("/work/steamapps/workshop/downloads/431960/999"),
                PathBuf::from("/work/steamapps/workshop/content/431960/999"),
            ]
        );
    }

    #[test]
    fn commit_stalled_artifact_moves_files() {
        let tmp = std::env::temp_dir().join(format!("wem-stall-{}", std::process::id()));
        let downloads = tmp.join("downloads");
        let content = tmp.join("content");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::write(downloads.join("scene.pkg"), b"pkg-bytes").unwrap();
        std::fs::write(downloads.join("project.json"), b"{}").unwrap();

        assert_eq!(commit_stalled_artifact(&downloads, &content).unwrap(), 2);
        assert!(content.join("scene.pkg").is_file());
        assert!(content.join("project.json").is_file());
        assert!(std::fs::read_dir(&downloads).unwrap().next().is_none());

        // downloads 不存在：返回 0 不报错；再次调用幂等（目录已空）
        assert_eq!(commit_stalled_artifact(&tmp.join("nope"), &content).unwrap(), 0);
        assert_eq!(commit_stalled_artifact(&downloads, &content).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn zero_hole_detects_preallocated_unwritten_regions() {
        let tmp = std::env::temp_dir().join(format!("wem-hole-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // 8MB 文件：中间 2MB 空洞（模拟 steamcmd 预分配未写完）
        let mut data = vec![0xABu8; 8 * 1024 * 1024];
        for b in data.iter_mut().skip(3 * 1024 * 1024).take(2 * 1024 * 1024) {
            *b = 0;
        }
        std::fs::write(tmp.join("scene.pkg"), &data).unwrap();
        assert!(has_zero_hole(&tmp, 1024 * 1024), "2MB 空洞必须被识别");

        // 低于阈值的长零段（700KB）不误报：正常 pkg 语料最大零段 ≤ 762KB
        let mut ok = vec![0xABu8; 8 * 1024 * 1024];
        for b in ok.iter_mut().skip(1024 * 1024).take(700 * 1024) {
            *b = 0;
        }
        std::fs::write(tmp.join("scene.pkg"), &ok).unwrap();
        assert!(!has_zero_hole(&tmp, 1024 * 1024), "700KB 零段不该误报");

        // 目录不存在：不得 panic
        assert!(!has_zero_hole(&tmp.join("nope"), 1024 * 1024));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn latest_mtime_tracks_writes() {
        let tmp = std::env::temp_dir().join(format!("wem-mtime-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        assert!(latest_mtime(&tmp, &[]).is_none(), "空目录没有 mtime");
        std::fs::write(tmp.join("sub").join("a.bin"), b"x").unwrap();
        let t1 = latest_mtime(&tmp, &[]).expect("写入后应有 mtime");
        // 再写一次：mtime 必须前进（steamcmd 预分配后靠它判「还在写」）
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(tmp.join("sub").join("b.bin"), b"y").unwrap();
        let t2 = latest_mtime(&tmp, &[]).unwrap();
        assert!(t2 >= t1);
        // skip 顶层目录：跳过 sub 后应看不到文件
        assert!(latest_mtime(&tmp, &["sub"]).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn matches_success_and_prompts() {
        let b = sc();
        assert!(b.match_success(r#"Success. Downloaded item 123 to "/x" (456 bytes)"#));
        assert!(!b.match_success("Downloading item 123 ..."));
        assert_eq!(
            b.match_guard_prompt("Steam Guard code:"),
            Some(GuardKind::Code)
        );
        assert_eq!(
            b.match_guard_prompt("two-factor code:"),
            Some(GuardKind::Code)
        );
        assert_eq!(
            b.match_guard_prompt("Please confirm the login in the Steam Mobile app on your phone."),
            Some(GuardKind::MobileConfirm)
        );
        assert_eq!(b.match_guard_prompt("password:"), Some(GuardKind::Password));
        assert_eq!(b.match_guard_prompt("Loading Steam API...OK"), None);
    }

    #[test]
    fn rate_limit_is_recognized_and_not_retryable() {
        let b = sc();
        // steamcmd / SteamKit 的两种写法都要能认出来：限流不能自动重试
        assert!(b.is_rate_limit("ERROR! Rate Limit Exceeded"));
        assert!(b.is_rate_limit("Too many login attempts. Please try again later."));
        assert!(!b.is_rate_limit("Logging in using cached credentials."));
        assert!(!b.is_rate_limit("Success. Downloaded item 123"));
        // 顺带锁住文案：这不是「网络抖动」，重试只会把限流窗口拖长
        assert_eq!(
            b.parse_failure("Rate Limit Exceeded").as_deref(),
            Some("登录过于频繁，请稍后再试")
        );
    }

    #[test]
    fn login_start_detects_password_login_only() {
        let b = sc();
        // console_log.txt 实测：密码登录的两个信号
        assert!(b.match_login_start("Logging in using username/password."));
        assert!(b.match_login_start("Logging in user 'xiaojian520520' [U:1:0] to Steam Public..."));
        // 缓存登录不触发：否则每次秒登录都会误弹「等待手机确认」
        assert!(!b.match_login_start("Logging in using cached credentials."));
        assert!(!b.match_login_start(
            "Logging in user 'xiaojian520520' [U:1:351868315] to Steam Public..."
        ));
        assert!(!b.match_login_start("Loading Steam API...OK"));
    }

    #[test]
    fn logging_in_user_tail_is_treated_as_prompt() {
        // steamcmd 打印该句后不带换行地阻塞等待确认，必须能从尾部文本里探测到
        assert!(is_prompt("Logging in user 'x' [U:1:0] to Steam Public..."));
        assert!(!is_prompt("Waiting for client config..."));
    }

    #[test]
    fn no_connection_mentions_both_causes() {
        let msg = sc()
            .parse_failure("ERROR! Download item 123 failed (No Connection).")
            .expect("应识别为失败");
        assert!(msg.contains("网络"));
        assert!(msg.contains("Wallpaper Engine"));
    }

    #[test]
    fn overrides_home_only() {
        let env = sc().extra_env();
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, crate::download::steamcmd_install::home_env_key());
        assert_eq!(env[0].1, OsString::from("/tmp/steamcmd-home"));
    }

    #[test]
    fn common_login_failures_are_mapped() {
        let b = sc();
        assert_eq!(
            b.parse_failure("Login Failure: Invalid Password")
                .as_deref(),
            Some("账号或密码错误")
        );
        assert!(b
            .parse_failure("ERROR! Download item 1 failed (No match).")
            .unwrap()
            .contains("不存在"));
        assert_eq!(b.parse_failure("Loading Steam API...OK"), None);
    }

    #[test]
    fn backend_setting_defaults_to_steamcmd() {
        // 缺省 / 历史残留 / 非法值一律 steamcmd：默认工具不能被脏设置改掉
        for raw in [
            None,
            Some(""),
            Some("steamcmd"),
            Some("DepotDownloader-old"),
        ] {
            assert_eq!(
                BackendKind::from_setting(raw),
                BackendKind::SteamCmd,
                "raw={raw:?}"
            );
        }
        assert_eq!(
            BackendKind::from_setting(Some("depotdownloader")),
            BackendKind::DepotDownloader
        );
        assert_eq!(BackendKind::SteamCmd.as_str(), "steamcmd");
        assert_eq!(BackendKind::DepotDownloader.as_str(), "depotdownloader");
        // 两个工具各自一套登录态标记
        assert_ne!(
            BackendKind::SteamCmd.token_setting(),
            BackendKind::DepotDownloader.token_setting()
        );
    }

    #[test]
    fn prompt_detector_switches_per_backend() {
        let steam = Backend::SteamCmd(sc());
        let dd = Backend::DepotDownloader(DepotDl {
            bin: PathBuf::from("/tmp/depotdownloader/DepotDownloader"),
            home: PathBuf::from("/tmp/depotdl-home"),
        });
        // steamcmd 的提示检测器认不出 DD 的提示，反之亦然（错配会让输入框弹不出来）
        assert!((steam.prompt_detector())("Steam Guard code:"));
        assert!(!(steam.prompt_detector())(
            "STEAM GUARD! Please enter your 2-factor auth code from your authenticator app: "
        ));
        assert!((dd.prompt_detector())(
            "STEAM GUARD! Please enter your 2-factor auth code from your authenticator app: "
        ));
        assert!(!(dd.prompt_detector())("Steam Guard code:"));
        // DD 不需要「密码登录已开始」的推测（提示是明文的）
        assert!(!dd.match_login_start("Logging in using username/password."));
        assert!(steam.match_login_start("Logging in using username/password."));
        // 退出码可信度与进度来源按后端区分
        assert!(!steam.trust_exit_code());
        assert!(dd.trust_exit_code());
        assert!(!steam.progress_from_output());
        assert!(dd.progress_from_output());
        assert_eq!(dd.sanitize_line("\u{1b}]9;4;1;5\u{7}hi".into()), "hi");
    }
}
