//! Steam 网页登录用的 RSA PKCS#1 v1.5 加密（GetPasswordRSAPublicKey → 加密密码）。
//!
//! 不引 rsa/num-bigint：2048 位模幂用「移位-加减」模乘实现，登录一次只跑
//! ~20 次模乘（指数恒为 65537），毫秒级完成，换来零新增依赖。
//! 数字表示：小端 u32 数组（v[0] 是最低 32 位），比较/减法按定长进行。

/// 大端字节 → 小端 u32 数组
fn from_be_bytes(b: &[u8]) -> Vec<u32> {
    let mut v = vec![0u32; b.len().div_ceil(4)];
    for (i, &byte) in b.iter().rev().enumerate() {
        v[i / 4] |= (byte as u32) << ((i % 4) * 8);
    }
    trim(&mut v);
    v
}

/// 小端 u32 数组 → 定长大端字节
fn to_be_bytes(v: &[u32], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    for i in 0..len {
        let limb = i / 4;
        if limb < v.len() {
            out[len - 1 - i] = (v[limb] >> ((i % 4) * 8)) as u8;
        }
    }
    out
}

fn trim(v: &mut Vec<u32>) {
    while v.len() > 1 && v.last() == Some(&0) {
        v.pop();
    }
}

/// a >= b（小端数组，长度可不同）
fn ge(a: &[u32], b: &[u32]) -> bool {
    let n = a.len().max(b.len());
    for i in (0..n).rev() {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    true
}

/// a - b（要求 a >= b），定长对齐
fn sub_assign(a: &mut Vec<u32>, b: &[u32]) {
    let mut borrow = 0i64;
    for i in 0..a.len() {
        let bi = b.get(i).copied().unwrap_or(0) as i64;
        let d = a[i] as i64 - bi - borrow;
        if d < 0 {
            a[i] = (d + (1i64 << 32)) as u32;
            borrow = 1;
        } else {
            a[i] = d as u32;
            borrow = 0;
        }
    }
    debug_assert_eq!(borrow, 0, "sub_assign 要求 a >= b");
    trim(a);
}

/// (a + b) mod m。前提 a,b < m（模乘的不变量保证）；a+b < 2m，至多减一次
fn addmod(a: &[u32], b: &[u32], m: &[u32]) -> Vec<u32> {
    let n = m.len() + 1; // 多一肢容纳进位：a+b < 2m ≤ 2^(32n)
    let mut out = vec![0u32; n];
    let mut carry = 0u64;
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0) as u64;
        let y = b.get(i).copied().unwrap_or(0) as u64;
        let s = x + y + carry;
        out[i] = s as u32;
        carry = s >> 32;
    }
    debug_assert_eq!(carry, 0);
    trim(&mut out);
    if ge(&out, m) {
        sub_assign(&mut out, m);
    }
    out
}

/// (a * b) mod m，俄式乘加（双模加），b 的每个置位累加一次。
/// 按整 limb 扫满 32 位：高位肢可能为 0，提前停会让 cur 的 2 次幂对不齐
fn modmul(a: &[u32], b: &[u32], m: &[u32]) -> Vec<u32> {
    let mut b = b.to_vec();
    trim(&mut b);
    let mut result = vec![0u32; 1];
    let mut cur = a.to_vec();
    for i in 0..b.len() {
        let mut limb = b[i];
        for _ in 0..32 {
            if limb & 1 == 1 {
                result = addmod(&result, &cur, m);
            }
            limb >>= 1;
            cur = addmod(&cur, &cur, m);
        }
    }
    result
}

/// base^exp mod m，平方-乘（指数按大端字节从最高位扫描）
fn modpow(base: &[u32], exp_be: &[u8], m: &[u32]) -> Vec<u32> {
    let mut result = vec![1u32];
    let mut started = false;
    for &byte in exp_be {
        for bit in (0..8).rev() {
            let b = (byte >> bit) & 1;
            if !started {
                if b == 0 {
                    continue;
                }
                started = true;
                result = base.to_vec();
                continue;
            }
            result = modmul(&result, &result, m);
            if b == 1 {
                result = modmul(&result, base, m);
            }
        }
    }
    result
}

/// RSA PKCS#1 v1.5 加密：EM = 00 02 PS 00 M（PS 为随机非零字节），
/// 输出定长（模数字节数）大端密文。
///
/// `pad` 为可注入的随机源：测试传确定性字节，生产用 rand::random。
pub fn encrypt_pkcs1v15(
    pubkey_mod_hex: &str,
    pubkey_exp_hex: &str,
    msg: &[u8],
    pad: &dyn Fn(usize) -> Vec<u8>,
) -> Result<Vec<u8>, String> {
    let m_bytes = hex_to_bytes(pubkey_mod_hex).ok_or("publickey_mod 不是合法 hex")?;
    let e_bytes = hex_to_bytes(pubkey_exp_hex).ok_or("publickey_exp 不是合法 hex")?;
    let k = m_bytes.len();
    if msg.len() + 11 > k {
        return Err("消息过长，无法做 PKCS#1 v1.5 填充".into());
    }
    // EM = 00 02 PS 00 M
    let ps_len = k - msg.len() - 3;
    let mut em = vec![0u8, 2u8];
    let mut ps = Vec::with_capacity(ps_len);
    while ps.len() < ps_len {
        // 填充字节必须全非零
        ps.extend(pad(ps_len - ps.len()).into_iter().filter(|&b| b != 0));
    }
    ps.truncate(ps_len);
    em.extend_from_slice(&ps);
    em.push(0);
    em.extend_from_slice(msg);

    let modulus = from_be_bytes(&m_bytes);
    let base = from_be_bytes(&em);
    if !ge(&modulus, &base) {
        return Err("内部错误：填充后消息不小于模数".into());
    }
    let c = modpow(&base, &e_bytes, &modulus);
    Ok(to_be_bytes(&c, k))
}

fn hex_to_bytes(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// 生产用随机填充源
pub fn random_pad(n: usize) -> Vec<u8> {
    (0..n).map(|_| rand::random::<u8>()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modmul_addmod_basic() {
        let m = from_be_bytes(&[0x01, 0x00, 0x00, 0x01]); // 2^24+1
        let a = from_be_bytes(&[0xff, 0xff]);
        let b = from_be_bytes(&[0xff, 0xff]);
        // 65535*65535 = 4294836225; mod (16777217) = 4294836225 - 255*16777217 = 4294836225-4278190335=16645890
        let c = modmul(&a, &b, &m);
        assert_eq!(to_be_bytes(&c, 4), (16645890u32).to_be_bytes());
    }

    #[test]
    fn modpow_small() {
        // 7^13 mod 1000 = ?
        let m = from_be_bytes(&1000u32.to_be_bytes());
        let base = from_be_bytes(&7u32.to_be_bytes());
        let c = modpow(&base, &13u32.to_be_bytes(), &m);
        let expect = 7u64.pow(13) % 1000;
        assert_eq!(to_be_bytes(&c, 2), (expect as u16).to_be_bytes());
    }

    /// 与系统 openssl 互验：用 openssl 生成 2048 位密钥，本实现加密（确定性填充），
    /// openssl 解密应还原原文。openssl 不可用时跳过（CI 容器一般有）。
    #[test]
    fn rsa_openssl_roundtrip() {
        let dir = std::env::temp_dir().join(format!("wpem-rsa-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("k.pem");
        let gen = std::process::Command::new("openssl")
            .args(["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048"])
            .arg("-out")
            .arg(&key)
            .output();
        let Ok(out) = gen else {
            eprintln!("openssl 不可用，跳过互验");
            return;
        };
        if !out.status.success() {
            eprintln!("openssl genpkey 失败，跳过互验: {:?}", String::from_utf8_lossy(&out.stderr));
            return;
        }
        // 导出 modulus / exponent（hex）
        let text = std::process::Command::new("openssl")
            .args(["pkey", "-in"])
            .arg(&key)
            .args(["-text", "-noout"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&text.stdout);
        let hex_block = |name: &str| -> Option<String> {
            let start = stdout.find(name)? + name.len();
            let rest = &stdout[start..];
            // modulus 块延续到下一个非缩进字段名（modulus/publicExponent 之间没有空行）
            let end = rest
                .find("\npublic")
                .or_else(|| rest.find("\n\n"))
                .unwrap_or(rest.len());
            let mut hexstr: String =
                rest[..end].chars().filter(|c| c.is_ascii_hexdigit()).collect();
            // openssl 为保证正数可能补一个前导 00 字节（2048 位模数打成 257 字节）
            if hexstr.len() % 4 == 2 && hexstr.starts_with("00") {
                hexstr = hexstr[2..].to_string();
            }
            Some(hexstr)
        };
        let modulus = hex_block("modulus:").expect("解析 modulus");
        // publicExponent 行形如 "publicExponent: 65537 (0x10001)"
        let exp_line = stdout
            .lines()
            .find(|l| l.contains("publicExponent"))
            .expect("解析 exponent");
        let mut exp_hex = exp_line
            .split("0x")
            .nth(1)
            .and_then(|s| s.split(')').next())
            .expect("exponent hex")
            .to_string();
        if exp_hex.len() % 2 != 0 {
            exp_hex = format!("0{exp_hex}");
        }

        let msg = b"steam-password-123";
        let ct = encrypt_pkcs1v15(&modulus, &exp_hex, msg, &|n| vec![0xAB; n]).unwrap();
        let ct_file = dir.join("ct.bin");
        std::fs::write(&ct_file, &ct).unwrap();
        let dec = std::process::Command::new("openssl")
            .args(["pkeyutl", "-decrypt", "-inkey"])
            .arg(&key)
            .args(["-in"])
            .arg(&ct_file)
            .args(["-pkeyopt", "rsa_padding_mode:pkcs1"])
            .output()
            .unwrap();
        assert!(
            dec.status.success(),
            "openssl 解密失败: {}",
            String::from_utf8_lossy(&dec.stderr)
        );
        assert_eq!(dec.stdout, msg);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 测试专用可见性（端到端 mock 解密验密用）
#[cfg(test)]
pub(crate) fn from_be_bytes_pub(b: &[u8]) -> Vec<u32> {
    from_be_bytes(b)
}
#[cfg(test)]
pub(crate) fn to_be_bytes_pub(v: &[u32], len: usize) -> Vec<u8> {
    to_be_bytes(v, len)
}
#[cfg(test)]
pub(crate) fn modpow_pub(base: &[u32], exp_be: &[u8], m: &[u32]) -> Vec<u32> {
    modpow(base, exp_be, m)
}
