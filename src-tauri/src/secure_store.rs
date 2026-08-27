//! 本地加密凭据存储（替代 macOS Keychain 作为首选，避免每次读取密码都触发系统授权）。
//!
//! 加密：随机 16B IV + 由固定盐派生的密钥生成 SHA-256 密钥流做 XOR（非明文落盘）。
//! 说明：密钥在本地应用内，主要目的是「不在磁盘明文保存密码 + 不依赖 Keychain 授权」，
//! 不等同于强加密。旧 Keychain 逻辑保留在 keychain.rs（作为回退/迁移来源）。

use sha2::{Digest, Sha256};
use std::path::Path;

const SALT: &[u8] = b"wallpaperem-local-credentials-v1";
const FILE: &str = "credentials.dat";

fn key() -> [u8; 32] {
    let d = Sha256::digest(SALT);
    let mut k = [0u8; 32];
    k.copy_from_slice(&d);
    k
}

/// 由 key + iv + 计数器派生的密钥流（XOR 流加密）
fn keystream(key: &[u8; 32], iv: &[u8; 16], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut counter: u64 = 0;
    while out.len() < len {
        let mut h = Sha256::new();
        h.update(key);
        h.update(iv);
        h.update(counter.to_le_bytes());
        out.extend_from_slice(h.finalize().as_ref());
        counter += 1;
    }
    out.truncate(len);
    out
}

fn encrypt(plain: &[u8]) -> (Vec<u8>, [u8; 16]) {
    let iv: [u8; 16] = rand::random();
    let ks = keystream(&key(), &iv, plain.len());
    let ct: Vec<u8> = plain.iter().zip(&ks).map(|(p, k)| p ^ k).collect();
    (ct, iv)
}

fn decrypt(iv: &[u8; 16], ct: &[u8]) -> Vec<u8> {
    let ks = keystream(&key(), iv, ct.len());
    ct.iter().zip(&ks).map(|(c, k)| c ^ k).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// 保存凭据（用户名明文 + 加密后的密码）到 <dir>/credentials.dat
pub fn save(username: &str, password: &str, dir: &Path) -> Result<(), String> {
    let (ct, iv) = encrypt(password.as_bytes());
    let content = format!("{username}\n{}\n{}\n", hex(&iv), hex(&ct));
    let file = dir.join(FILE);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&file, content).map_err(|e| e.to_string())
}

/// 读取凭据（用户名匹配时返回解密后的密码）；不存在/不匹配返回 None
pub fn load(username: &str, dir: &Path) -> Result<Option<String>, String> {
    let data = match std::fs::read_to_string(dir.join(FILE)) {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };
    let mut lines = data.lines();
    let stored_user = lines.next().unwrap_or("");
    if stored_user != username {
        return Ok(None);
    }
    let iv = lines
        .next()
        .and_then(unhex)
        .and_then(|v| <[u8; 16]>::try_from(v).ok());
    let ct = lines.next().and_then(unhex);
    match (iv, ct) {
        (Some(iv), Some(ct)) => {
            let plain = decrypt(&iv, &ct);
            Ok(Some(String::from_utf8_lossy(&plain).into_owned()))
        }
        _ => Ok(None),
    }
}

/// 该用户名是否已配置本地凭据
pub fn has(username: &str, dir: &Path) -> bool {
    load(username, dir).ok().flatten().is_some()
}

/// 清除本地凭据文件（登出）
pub fn clear_all(dir: &Path) -> Result<(), String> {
    let file = dir.join(FILE);
    match std::fs::remove_file(&file) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("we-sec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn roundtrip() {
        let d = tmpdir();
        save("test@user", "s3cret!p@ss", &d).unwrap();
        let got = load("test@user", &d).unwrap();
        assert_eq!(got.as_deref(), Some("s3cret!p@ss"));
        assert!(has("test@user", &d));
        assert!(!has("other@user", &d));
        let _ = std::fs::remove_dir_all(&d);
    }
}
