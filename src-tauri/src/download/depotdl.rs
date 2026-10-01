//! DepotDownloader 后端：命令行拼装、输出解析、产物布局。
//!
//! 与 `backend.rs`（steamcmd）并列，可被设置页切换。与 steamcmd 的差异，
//! 全部来自读上游源码 + 本机实测（DepotDownloader 3.4.0，.NET 9 单文件）：
//!
//! - **没有 `-configdir`**：登录令牌落在 .NET 的 IsolatedStorage（按程序集隔离），
//!   `account.config` 则按**相对 CWD** 读写。隔离它要按平台各给一个环境变量
//!   （macOS 得用 `CFFIXED_USER_HOME` —— `HOME` 对 CoreFoundation 路径解析无效，
//!   实测令牌会漏进真实用户库），外加把子进程 CWD 设到应用目录：
//!   见 `extra_env` 与 `current_dir`。
//! - **有进度输出**：逐文件 ` 99.66% /path/file`（宽度 6 的百分比 + 路径），
//!   不像 steamcmd 那样全程静默，故进度直接来自输出而不是轮询目录体积。
//! - **退出码可信**：失败路径 `return 1`，成功正常退出（steamcmd 在 macOS 上
//!   下载完常卡在 Steam API 拆卸而不退出，只能靠成功行主动收尾）。
//! - **没有「只登录不下载」模式**：`-app` 是必填项，登录验证任务改用
//!   `-manifest-only`（只取清单不取文件）：既能验证登录，又能顺带验证
//!   「该账号拥有 WE」，且不落任何内容文件。
//! - **并发实例要各自不同的 `-loginid`**：上游 README 明确写了并发场景需要它，
//!   同一账号并行下载时由调用方按 task_id 生成，避免互相顶掉登录态。
//! - **提示写在 stderr**：`Console.Error.Write` 不带换行（验证码/邮箱码），
//!   手机确认提示带换行；两者都靠 pty 合并流收到。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::download::APP_ID;

use super::backend::{Credentials, GuardKind};

/// 工坊条目下载（`-pubfile`）成功行：
/// `Total downloaded: 1234 bytes (5678 bytes uncompressed) from 1 depots`
const SUCCESS_RE: &str = r"(?i)Total downloaded:\s*\d+ bytes";
/// 登录成功行：`Got 3 licenses for account!`（LicenseListCallback，每次登录成功都会打）
const LOGIN_SUCCESS_RE: &str = r"(?i)Got \d+ licenses for account!";
/// 逐文件进度行：` 99.66% /path/to/file`
const PROGRESS_RE: &str = r"^\s*(\d+(?:\.\d+)?)%\s+\S";
/// 验证码提示（四条都是「请用户输入码」的提示尾部，不带换行）：
/// - `Please enter your 2-factor auth code from your authenticator app: `（ConsoleAuthenticator）
/// - `Please enter the auth code sent to the email at x@y: `（同上，邮箱码）
/// - `Please enter your 2 factor auth code from your authenticator app: `（旧版 Steam3 路径）
/// - `Please enter the authentication code sent to your email address: `（同上）
///
/// 只认 “enter (your|the) …code” 句式：错误重试时的 "The previous … code you have
/// provided is incorrect." 是另一类信息，走 parse_failure 而不是弹输入框。
const CODE_RE: &str = r"(?i)enter (your |the )?(2.?factor( auth)?|authentication|auth)\s*code";
/// 密码提示（登录态失效、又没带密码时会索要，正常流程不会出现）
const PASSWORD_RE: &str = r"(?i)enter account password for";
/// 手机确认：`STEAM GUARD! Use the Steam Mobile App to confirm your sign in...`
const MOBILE_RE: &str =
    r"(?i)(use the steam mobile app to confirm|confirm the login in the steam mobile app)";
/// 失败行（验证码输错后重新要码时先打这句，按失败收尾让用户重试更清楚）
const BAD_CODE_RE: &str = r"(?i)previous 2-factor auth code you have provided is incorrect|previous .{0,30}code .{0,20}is incorrect";

/// 看门狗与 steamcmd 共用同一量级：DD 输出频繁（每次进度都会重置），
/// 真正静默只可能出现在 CDN 卡死时。
#[derive(Clone)]
pub struct DepotDl {
    /// `<app_data>/depotdownloader/DepotDownloader[.exe]`
    pub bin: PathBuf,
    /// 重定向给子进程的 HOME（IsolatedStorage 登录令牌 + 相对 CWD 的 account.config）
    pub home: PathBuf,
}

impl DepotDl {
    pub fn is_installed(&self) -> bool {
        self.bin.is_file()
    }

    /// 隔离登录态用的环境变量。
    ///
    /// DD 的令牌存在 .NET 的 **IsolatedStorage** 里，根目录 = `LocalApplicationData/IsolatedStorage`，
    /// 而这一步各平台的解析方式完全不同（2026-10-01 实测 + 读 dotnet/runtime 源码）：
    ///
    /// - **macOS**：走 CoreFoundation 的 `NSApplicationSupportDirectory`（`Interop.Sys.SearchPath`），
    ///   **`HOME` 对它无效** —— 实测把 HOME 重定向到隔离目录后，令牌照样写进真实的
    ///   `~/Library/Application Support/IsolatedStorage/…`（多级混淆目录，路径算不出来）。
    ///   能覆盖它的是 `CFFIXED_USER_HOME`（CF 的官方家目录覆盖点，实测生效）。
    /// - **Linux**：`$XDG_DATA_HOME` 或 `$HOME/.local/share`（源码明写），给 XDG 即可。
    /// - **Windows**：`SHGetKnownFolderPath` 读注册表，改 `USERPROFILE` 无效 —— 这里隔离不了，
    ///   只能靠登出时按时间窗清扫（见 [`sweep_token_dirs`]）。
    pub fn extra_env(&self) -> Vec<(String, OsString)> {
        let mut env = vec![(
            crate::download::steamcmd_install::home_env_key().to_string(),
            self.home.as_os_str().into(),
        )];
        env.push((
            "XDG_DATA_HOME".to_string(),
            self.home.join(".local").join("share").into_os_string(),
        ));
        if cfg!(target_os = "macos") {
            env.push((
                "CFFIXED_USER_HOME".to_string(),
                self.home.as_os_str().into(),
            ));
        }
        env
    }

    /// 子进程工作目录：`account.config`（DD 按相对路径读写）落在这里，
    /// 同时保证 DEFAULT_DOWNLOAD_DIR（未给 -dir 时的默认目录）不会跑进用户家目录。
    pub fn current_dir(&self) -> Option<PathBuf> {
        Some(self.home.clone())
    }

    /// 单个工坊条目的命令行参数（不含程序自身）。
    /// `-app` 在 `-pubfile` 模式下同样是必填的（上游用它查 workshopdepot）。
    pub fn build_args(
        &self,
        item_id: &str,
        workdir: &Path,
        cred: &Credentials,
        login_id: i64,
    ) -> Vec<OsString> {
        let mut args = self.auth_args(cred, login_id);
        args.push("-app".into());
        args.push(APP_ID.into());
        args.push("-pubfile".into());
        args.push(item_id.into());
        args.push("-dir".into());
        args.push(workdir.into());
        args
    }

    /// 登录验证：没有纯登录模式，用「只取 App 清单」代替 —— 只走登录 + 清单查询，
    /// 不下载任何内容文件，同时能验证该账号是否有权访问 WE。
    pub fn build_login_args(
        &self,
        workdir: &Path,
        cred: &Credentials,
        login_id: i64,
    ) -> Vec<OsString> {
        let mut args = self.auth_args(cred, login_id);
        args.push("-app".into());
        args.push(APP_ID.into());
        args.push("-manifest-only".into());
        args.push("-dir".into());
        args.push(workdir.into());
        args
    }

    /// 公共认证段。
    ///
    /// `-remember-password` 与 `-username` 一起用时 DD 会记住刷新令牌，
    /// 下次只给账号即可免密（与 steamcmd 的缓存登录态对应）；
    /// `-loginid` 按任务区分，避免同账号并行实例互相顶掉会话。
    fn auth_args(&self, cred: &Credentials, login_id: i64) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "-username".into(),
            cred.username.as_str().into(),
            "-remember-password".into(),
            "-loginid".into(),
            login_id.to_string().into(),
        ];
        if !cred.has_token {
            args.push("-password".into());
            args.push(cred.password.as_str().into());
        }
        args
    }

    /// 该行输出是否表示登录成功（验证任务用；普通任务用它解除手机确认推测）
    pub fn match_login_success(&self, s: &str) -> bool {
        re(LOGIN_SUCCESS_RE).is_match(s)
    }

    /// 该行输出是否表示下载成功
    pub fn match_success(&self, s: &str) -> bool {
        re(SUCCESS_RE).is_match(s)
    }

    /// 从输出行解析下载百分比（逐文件 ` 99.66% /path`）
    pub fn parse_progress(&self, s: &str) -> Option<f64> {
        re(PROGRESS_RE)
            .captures(s)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<f64>().ok())
    }

    /// 该行输出是否在索要交互输入
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

    /// 该行输出是否表示失败，返回面向用户的中文原因。
    ///
    /// 只认上游源码里明确的失败文案：DD 失败会 return 1，退出码本身已可信，
    /// 这里主要把「连接/权限/验证码」三类原因翻成人话。
    pub fn parse_failure(&self, s: &str) -> Option<String> {
        let l = s.to_ascii_lowercase();
        if re(BAD_CODE_RE).is_match(s) {
            return Some("验证码错误，请重新输入".into());
        }
        // 账号未拥有该 App（下载与验证都可能是这条）：两种写法都见过
        if l.contains("is not available from this account") {
            if l.contains("app ") {
                return Some("该账号无法获取 Wallpaper Engine（未拥有，或所在区服不可用）".into());
            }
            // "Depot 123 is not available from this account." 只是跳过该 depot，
            // 交给退出码/产物判定，不在这里判死刑
            return None;
        }
        if l.contains("invalidpassword") || l.contains("invalid password") {
            return Some("账号或密码错误".into());
        }
        if l.contains("ratelimitexceeded") || l.contains("rate limit") {
            return Some("登录过于频繁，请稍后再试".into());
        }
        if l.contains("failed to authenticate with steam") {
            return Some(format!(
                "Steam 登录失败：{}",
                s.chars().take(160).collect::<String>()
            ));
        }
        if l.contains("access token was rejected") {
            return Some("登录态已失效，请到「设置 → 账号」重新登录".into());
        }
        if l.contains("unable to get steam3 credentials") {
            return Some("登录未通过（未取到 Steam 凭据），请检查账号密码或稍后重试".into());
        }
        if l.contains("unable to login to steam3") {
            return Some(format!(
                "Steam 登录被拒绝：{}",
                s.chars().take(160).collect::<String>()
            ));
        }
        if l.contains("unable to locate manifest id for published file") {
            return Some("该工坊条目不存在或已被作者删除".into());
        }
        if l.contains("has unsupported file type") {
            return Some("该工坊条目类型不受支持".into());
        }
        if l.contains("unable to download manifest")
            || l.contains("failed to find any server with chunk")
        {
            return Some("下载失败（Steam CDN 取清单/分片失败），请重试或检查网络/代理".into());
        }
        if l.contains("connection timeout") {
            return Some("下载超时，请重试或检查网络/代理".into());
        }
        if l.contains("could not connect to steam after") {
            return Some("无法连接 Steam 服务器，请检查网络/代理".into());
        }
        if l.contains("no valid depot key for") {
            return Some("无权访问该内容（需登录拥有 Wallpaper Engine 的账号）".into());
        }
        None
    }

    /// 产物落地目录：`-dir` 直接指定工作目录，内容就写在根下
    /// （元数据在 `.DepotDownloader/` 子目录里，收编时跳过）。
    pub fn artifact_dir(&self, workdir: &Path, _item_id: &str) -> PathBuf {
        workdir.to_path_buf()
    }

    /// 产物是否就绪：排除 `.DepotDownloader/`（depot.config 等元数据）后还要有内容，
    /// 否则「登录成功但一条内容都没下到」会被误判为成功。
    pub fn artifact_ready(&self, workdir: &Path) -> bool {
        super::backend::dir_size_excluding(workdir, &[".DepotDownloader"]) > 0
    }

    /// 收编时要跳过的元数据目录名
    pub fn is_metadata_entry(&self, name: &str) -> bool {
        name == ".DepotDownloader"
    }

    /// 退出码可信（见模块头注释）：DD 失败必 return 1，成功正常退出。
    pub fn trust_exit_code(&self) -> bool {
        true
    }

    /// 隔离目录里是否真有登录令牌。
    ///
    /// 用来给「登录态标记」做体检：DB 里的 `depotdl_has_token` 只是我们上一次成功后写下的
    /// 乐观标记，而令牌可能已经不在（用户删了目录、从没在这个位置登录过、Windows 上压根
    /// 不落这儿）。令牌不在就把密码带上，让 DD 走一次密码登录（它自己会在拿到令牌后
    /// 优先用令牌，不会多要一次验证码）。
    pub fn has_stored_session(&self) -> bool {
        token_store_present(&self.home)
    }
}

/// `home` 下是否存在 DD 写的 `account.config`（IsolatedStorage 的叶子文件）。
/// 深度封顶：`…/IsolatedStorage/<混淆>/<混淆>/Url.<hash>/AssemFiles/account.config`。
pub fn token_store_present(home: &Path) -> bool {
    fn walk(dir: &Path, depth: usize) -> bool {
        if depth > 6 {
            return false;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if walk(&p, depth + 1) {
                    return true;
                }
            } else if p
                .file_name()
                .map(|n| n == "account.config")
                .unwrap_or(false)
            {
                return true;
            }
        }
        false
    }
    walk(home, 0)
}

/// 无需构造实例的提示判定，供字节级读取器的 'static 闭包使用。
pub fn is_prompt(tail: &str) -> bool {
    re(CODE_RE).is_match(tail) || re(MOBILE_RE).is_match(tail) || re(PASSWORD_RE).is_match(tail)
}

/// 平台上的 **真实** .NET IsolatedStorage 根（不是我们重定向后的那份）。
///
/// 只给登出清扫用：macOS/Linux 上令牌已经被环境变量关进应用目录（整目录随登出删除），
/// Windows 上 `SHGetKnownFolderPath` 不听环境变量的，令牌就留在真实用户库 —— 这一份要单独扫。
pub fn real_isolated_storage_root() -> Option<PathBuf> {
    let home = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?
    } else {
        // .NET 在 Darwin 用 NSApplicationSupportDirectory，在 Linux 用 XDG/HOME
        let xdg = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        match (cfg!(target_os = "macos"), xdg) {
            (true, _) => dirs::home_dir()?
                .join("Library")
                .join("Application Support"),
            (false, Some(p)) => p,
            (false, None) => dirs::home_dir()?.join(".local").join("share"),
        }
    };
    Some(home.join("IsolatedStorage"))
}

/// 扫描 IsolatedStorage 里**本次登录之后**被写过的 DD 令牌目录，返回其所属身份目录
/// （`<…>/Url.<hash>`，即 `AssemFiles/account.config` 的父目录）。
///
/// 只认「目录结构 + 文件名 + 时间窗」三件事，不做任何路径猜测 —— IsolatedStorage 的
/// 目录名是按机器密钥混淆过的，算不出来，所以按「我们自己刚写出来的那份」定位：
/// 时间窗取我们记录的成功登录时刻（见 `depotdl_token_at`），别的 .NET 应用不会在这几秒里
/// 恰好写一个同名文件。深度也封顶（`Url.*/AssemFiles/account.config` 是固定三层）。
pub fn sweep_token_dirs(root: &Path, since: std::time::SystemTime) -> Vec<PathBuf> {
    const TOKEN_FILE: &str = "account.config";
    fn walk(dir: &Path, depth: usize, since: std::time::SystemTime, out: &mut Vec<PathBuf>) {
        if depth > 4 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, depth + 1, since, out);
            } else if p.file_name().map(|n| n == TOKEN_FILE).unwrap_or(false) {
                let fresh = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|t| t >= since)
                    .unwrap_or(false);
                if fresh {
                    if let Some(owner) = p.parent().and_then(|f| f.parent()) {
                        if !out.contains(&owner.to_path_buf()) {
                            out.push(owner.to_path_buf());
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, 0, since, &mut out);
    out
}

/// 删掉这些身份目录里的令牌文件（`account.config` 及其所在的 `AssemFiles/`）。
/// 返回删掉的目录数；失败只记日志，不打断登出。
pub fn remove_token_dirs(dirs: &[PathBuf]) -> usize {
    let mut n = 0;
    for d in dirs {
        if std::fs::remove_file(d.join("AssemFiles").join("account.config")).is_ok() {
            let _ = std::fs::remove_dir(d.join("AssemFiles"));
            let _ = std::fs::remove_dir(d);
            n += 1;
        }
    }
    n
}

/// 去掉 ANSI / OSC 控制序列（DD 在支持的终端上会写 `ESC]9;4;…BEL` 进度条指令，
/// 这些字节不带换行，会一直积在读取器的尾部缓冲里）。
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match it.peek() {
            // OSC：ESC ] … BEL（或 ESC \）
            Some(']') => {
                it.next();
                while let Some(&n) = it.peek() {
                    it.next();
                    if n == '\u{7}' {
                        break;
                    }
                    if n == '\u{1b}' {
                        if it.peek() == Some(&'\\') {
                            it.next();
                        }
                        break;
                    }
                }
            }
            // CSI：ESC [ … 结束字节（0x40..=0x7E）
            Some('[') => {
                it.next();
                while let Some(&n) = it.peek() {
                    it.next();
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        break;
                    }
                }
            }
            // 其余单字符转义
            _ => {
                it.next();
            }
        }
    }
    out
}

/// 编译并缓存正则（与 steamcmd 后端同款实现，避免每行输出现场编译）。
fn re(pattern: &'static str) -> &'static regex::Regex {
    super::backend::cached_regex(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dd() -> DepotDl {
        DepotDl {
            bin: PathBuf::from("/tmp/depotdownloader/DepotDownloader"),
            home: PathBuf::from("/tmp/depotdl-home"),
        }
    }

    fn args_of(prog: DepotDl, item: &str, token: bool, login_id: i64) -> Vec<String> {
        let cred = Credentials {
            username: "alice".into(),
            password: if token { String::new() } else { "pw".into() },
            has_token: token,
        };
        prog.build_args(item, Path::new("/work"), &cred, login_id)
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn download_args_need_app_and_pubfile() {
        let args = args_of(dd(), "123", false, 7);
        assert!(args.starts_with(&["-username".into(), "alice".into()]));
        assert!(args.contains(&"-remember-password".to_string()));
        let app_at = args.iter().position(|a| a == "-app").unwrap();
        assert_eq!(args[app_at + 1], APP_ID);
        let pub_at = args.iter().position(|a| a == "-pubfile").unwrap();
        assert_eq!(args[pub_at + 1], "123");
        let dir_at = args.iter().position(|a| a == "-dir").unwrap();
        assert_eq!(args[dir_at + 1], "/work");
        let li = args.iter().position(|a| a == "-loginid").unwrap();
        assert_eq!(args[li + 1], "7");
        let pw = args.iter().position(|a| a == "-password").unwrap();
        assert_eq!(args[pw + 1], "pw");
    }

    #[test]
    fn args_omit_password_when_token_present() {
        let args = args_of(dd(), "123", true, 1);
        assert!(!args.contains(&"-password".to_string()));
        assert!(args.contains(&"-remember-password".to_string()));
    }

    #[test]
    fn login_args_use_manifest_only() {
        let cred = Credentials {
            username: "alice".into(),
            password: "pw".into(),
            has_token: false,
        };
        let args: Vec<String> = dd()
            .build_login_args(Path::new("/w"), &cred, 9)
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(args.contains(&"-manifest-only".to_string()));
        assert!(!args.contains(&"-pubfile".to_string()));
        assert_eq!(
            args[args.iter().position(|a| a == "-app").unwrap() + 1],
            APP_ID
        );
    }

    #[test]
    fn artifact_is_workdir_and_metadata_excluded() {
        let d = std::env::temp_dir().join("wpem-depotdl-artifact-test");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".DepotDownloader")).unwrap();
        std::fs::write(d.join(".DepotDownloader").join("depot.config"), "meta").unwrap();
        let prog = dd();
        assert_eq!(prog.artifact_dir(&d, "123"), d);
        // 只有元数据 → 不算有产物
        assert!(!prog.artifact_ready(&d));
        std::fs::write(d.join("scene.pkg"), "content").unwrap();
        assert!(prog.artifact_ready(&d));
        assert!(prog.is_metadata_entry(".DepotDownloader"));
        assert!(!prog.is_metadata_entry("scene.pkg"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn matches_success_login_and_progress() {
        let p = dd();
        assert!(p.match_success(
            "Total downloaded: 2256904 bytes (11323368 bytes uncompressed) from 1 depots"
        ));
        assert!(!p.match_success("Downloading scene.pkg"));
        assert!(p.match_login_success("Got 3 licenses for account!"));
        assert_eq!(p.parse_progress(" 99.66% /materials/tex.png"), Some(99.66));
        assert_eq!(p.parse_progress("  0.00% /scene.pkg"), Some(0.0));
        assert_eq!(p.parse_progress("Processing depot 431962"), None);
    }

    #[test]
    fn guard_prompts_from_real_output() {
        let p = dd();
        // ConsoleAuthenticator（stderr，无换行）
        assert_eq!(
            p.match_guard_prompt(
                "STEAM GUARD! Please enter your 2-factor auth code from your authenticator app: "
            ),
            Some(GuardKind::Code)
        );
        assert_eq!(
            p.match_guard_prompt(
                "STEAM GUARD! Please enter the auth code sent to the email at a@b.com: "
            ),
            Some(GuardKind::Code)
        );
        // 旧版 Steam3 路径（stdout）
        assert_eq!(
            p.match_guard_prompt(
                "Please enter your 2 factor auth code from your authenticator app: "
            ),
            Some(GuardKind::Code)
        );
        assert_eq!(
            p.match_guard_prompt(
                "Please enter the authentication code sent to your email address: "
            ),
            Some(GuardKind::Code)
        );
        assert_eq!(
            p.match_guard_prompt(
                "STEAM GUARD! Use the Steam Mobile App to confirm your sign in..."
            ),
            Some(GuardKind::MobileConfirm)
        );
        assert_eq!(
            p.match_guard_prompt("Enter account password for \"alice\": "),
            Some(GuardKind::Password)
        );
        // 输错码的提示不该再弹输入框
        assert_eq!(
            p.match_guard_prompt("The previous 2-factor auth code you have provided is incorrect."),
            None
        );
        assert_eq!(
            p.match_guard_prompt("Got manifest request code for depot 431962 from app 431960"),
            None
        );
    }

    #[test]
    fn tail_prompts_detected() {
        assert!(is_prompt(
            "STEAM GUARD! Please enter your 2-factor auth code from your authenticator app: "
        ));
        assert!(is_prompt("Enter account password for \"alice\": "));
        assert!(!is_prompt("Connecting to Steam3..."));
    }

    #[test]
    fn failures_mapped_to_chinese() {
        let p = dd();
        assert_eq!(
            p.parse_failure("Failed to authenticate with Steam: Authentication failed with result InvalidPassword.").as_deref(),
            Some("账号或密码错误")
        );
        assert!(p
            .parse_failure("App 431960 (Wallpaper Engine) is not available from this account.")
            .unwrap()
            .contains("未拥有"));
        // depot 级不可用只是跳过，不能判整个任务失败
        assert_eq!(
            p.parse_failure("Depot 431962 is not available from this account."),
            None
        );
        assert!(p
            .parse_failure("Unable to locate manifest ID for published file 123")
            .unwrap()
            .contains("不存在"));
        assert_eq!(
            p.parse_failure("Access token was rejected (Expired)."),
            Some("登录态已失效，请到「设置 → 账号」重新登录".into())
        );
        assert_eq!(
            p.parse_failure("The previous 2-factor auth code you have provided is incorrect."),
            Some("验证码错误，请重新输入".into())
        );
        assert_eq!(p.parse_failure("Disconnected from Steam"), None);
        assert_eq!(p.parse_failure("Connecting to Steam3... Done!"), None);
    }

    #[test]
    fn token_store_probe_finds_account_config_only() {
        let d = std::env::temp_dir().join("wpem-depotdl-token-probe");
        let _ = std::fs::remove_dir_all(&d);
        // IsolatedStorage 的形态：<混淆>/<混淆>/Url.<hash>/AssemFiles/account.config
        let leaf = d
            .join("5gaj3azb.qtq")
            .join("jep5swvl.5pu")
            .join("Url.02eo5hacpemcgklat33fbjrszovmtsh3")
            .join("AssemFiles");
        std::fs::create_dir_all(&leaf).unwrap();
        assert!(!token_store_present(&d), "没有令牌文件时应为 false");
        std::fs::write(leaf.join("account.config"), b"x").unwrap();
        assert!(token_store_present(&d));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sweep_picks_fresh_token_dirs_and_removes_them() {
        let root = std::env::temp_dir().join("wpem-depotdl-token-sweep");
        let _ = std::fs::remove_dir_all(&root);
        let mk = |name: &str| {
            let leaf = root.join("obf1").join("obf2").join(name).join("AssemFiles");
            std::fs::create_dir_all(&leaf).unwrap();
            std::fs::write(leaf.join("account.config"), b"token").unwrap();
            leaf.parent().unwrap().to_path_buf()
        };
        let mine = mk("Url.aaaa");
        // 别的目录（结构不同）与别的文件名都不该被认领
        std::fs::create_dir_all(
            root.join("obf1")
                .join("obf2")
                .join("Other")
                .join("AssemFiles"),
        )
        .unwrap();
        std::fs::write(
            root.join("obf1")
                .join("obf2")
                .join("Other")
                .join("AssemFiles")
                .join("depot.config"),
            b"x",
        )
        .unwrap();

        // 时间窗在未来 → 全都不算「刚写出来的」
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(600);
        assert!(sweep_token_dirs(&root, future).is_empty());
        // 时间窗在过去（带 5 分钟余量，与登出路径同款算法）→ 认领并删除
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(300);
        assert_eq!(sweep_token_dirs(&root, past), vec![mine.clone()]);
        assert_eq!(remove_token_dirs(&[mine.clone()]), 1);
        assert!(!mine.exists());
        // 无关目录原样保留
        assert!(root
            .join("obf1")
            .join("obf2")
            .join("Other")
            .join("AssemFiles")
            .join("depot.config")
            .is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn env_redirects_home_and_native_store() {
        // 只断言「我们给了哪些键」：值故意取隔离目录本身，不碰真实家目录
        let d = DepotDl {
            bin: PathBuf::from("/tmp/depotdownloader/DepotDownloader"),
            home: PathBuf::from("/tmp/depotdl-home"),
        };
        let env = d.extra_env();
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&crate::download::steamcmd_install::home_env_key()));
        assert!(keys.contains(&"XDG_DATA_HOME"));
        if cfg!(target_os = "macos") {
            // macOS 上 .NET 走 CoreFoundation，HOME 无效，必须给 CFFIXED_USER_HOME
            assert!(keys.contains(&"CFFIXED_USER_HOME"), "{keys:?}");
        }
        assert!(
            !keys.contains(&"LOCALAPPDATA"),
            "Windows 侧隔离不了，别给假的保证"
        );
    }

    #[test]
    fn ansi_sequences_stripped() {
        assert_eq!(
            strip_ansi("\u{1b}]9;4;1;42\u{7}Downloading a.pkg"),
            "Downloading a.pkg"
        );
        assert_eq!(strip_ansi("\u{1b}[1;32mDone\u{1b}[0m"), "Done");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn workdir_is_the_isolated_home() {
        assert_eq!(dd().current_dir(), Some(PathBuf::from("/tmp/depotdl-home")));
    }
}
