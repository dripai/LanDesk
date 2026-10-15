//! Connection passwords are independent of operating-system accounts.
use anyhow::Result;
use anyhow::ensure;
use zeroize::Zeroizing;

pub fn generate() -> Result<Zeroizing<String>> {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut password = Zeroizing::new(String::with_capacity(8));
    while password.len() < 8 {
        let mut random = Zeroizing::new([0u8; 16]);
        getrandom::fill(random.as_mut()).map_err(|e| anyhow::anyhow!("生成连接密码失败: {e}"))?;
        for byte in random.iter().copied().filter(|byte| *byte < 248) {
            password.push(ALPHABET[usize::from(byte) % ALPHABET.len()] as char);
            if password.len() == 8 {
                break;
            }
        }
    }
    Ok(password)
}

pub fn validate(password: &str) -> Result<()> {
    ensure!(
        !password.is_empty() && password.len() <= 1024 && !password.contains('\0'),
        "连接密码不能为空，不能包含 NUL，且不能超过 1024 字节"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn generated_passwords_are_eight_ascii_letters_or_digits() {
        let mut unique = std::collections::HashSet::new();
        for _ in 0..256 {
            let password = super::generate().unwrap();
            assert_eq!(password.len(), 8);
            assert!(password.bytes().all(|byte| byte.is_ascii_alphanumeric()));
            assert!(unique.insert(password.to_string()));
        }
    }
}
