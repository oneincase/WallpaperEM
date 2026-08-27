//! macOS Keychain 凭据存取（下载账号密码，不落磁盘明文）

use keyring::Entry;

const SERVICE: &str = "com.oneincase.wallpaperem";

fn entry(username: &str) -> Result<Entry, String> {
    Entry::new(SERVICE, username).map_err(|e| format!("Keychain 初始化失败: {e}"))
}

#[allow(dead_code)]
pub fn set_password(username: &str, password: &str) -> Result<(), String> {
    entry(username)?
        .set_password(password)
        .map_err(|e| format!("Keychain 写入失败: {e}"))
}

pub fn get_password(username: &str) -> Result<String, String> {
    entry(username)?
        .get_password()
        .map_err(|e| format!("Keychain 读取失败: {e}"))
}

#[allow(dead_code)]
pub fn delete_password(username: &str) -> Result<(), String> {
    entry(username)?
        .delete_credential()
        .map_err(|e| format!("Keychain 删除失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 Keychain 写入后可读回（应用重启后仍应有效）
    #[test]
    fn keychain_set_get_roundtrip() {
        let user = format!("test-user-{}", std::process::id());
        set_password(&user, "s3cret-test-pw").expect("set_password 失败");
        let got = get_password(&user).expect("get_password 失败");
        assert_eq!(got, "s3cret-test-pw", "读回密码不一致");
        let _ = delete_password(&user);
    }
}
