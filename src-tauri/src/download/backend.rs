//! steamcmd 后端：命令行拼装、输出解析、产物布局。
//!
//! 队列、状态机、产物收编、依赖补拉由 `download/mod.rs` 承担，本模块只管
//! 「怎么跟 steamcmd 这个子进程打交道」。三个非显然的约束（都是踩过的坑）：
//!
//! - **stdin 必须是 TTY**：管道会让 steamcmd 直接打印
//!   "cannot read from the console" 退出 → 见 `pty.rs`
//! - **下载成功后常不退出**：macOS 上 Steam API 拆卸有线程竞争，`+quit` 无响应，
//!   必须靠 `match_success` 匹配到成功行后主动杀进程，不能等退出码
//! - **下载期间零进度输出**：`workshop_download_item` 只在首尾各打一行，
//!   中间完全静默 → 进度由 mod.rs 轮询产物目录体积估算

use std::ffi::OsString;
use std::path::{Path, PathBuf};

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
const LOGIN_START_RE: &str =
    r"(?i)logging in using username/password|logging in user .*\[u:1:0\]";

/// 工坊大件常见，且 steamcmd 下载期间完全静默，看门狗必须给足时间
pub const WATCHDOG: std::time::Duration = std::time::Duration::from_secs(30 * 60);

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
fn re(pattern: &'static str) -> &'static regex::Regex {
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

/// 递归统计目录内的字节数（steamcmd 无进度输出时用它估算下载进度）。
pub fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0u64;
    for e in entries.flatten() {
        match e.file_type() {
            Ok(t) if t.is_dir() => total += dir_size(&e.path()),
            Ok(t) if t.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => {}
        }
    }
    total
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
    fn login_start_detects_password_login_only() {
        let b = sc();
        // console_log.txt 实测：密码登录的两个信号
        assert!(b.match_login_start("Logging in using username/password."));
        assert!(
            b.match_login_start("Logging in user 'xiaojian520520' [U:1:0] to Steam Public...")
        );
        // 缓存登录不触发：否则每次秒登录都会误弹「等待手机确认」
        assert!(!b.match_login_start("Logging in using cached credentials."));
        assert!(!b
            .match_login_start("Logging in user 'xiaojian520520' [U:1:351868315] to Steam Public..."));
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
}
