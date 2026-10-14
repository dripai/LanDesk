//! The client and server compile this same AES-256-GCM implementation.
//! Keys are supplied by the caller, never derived from an embedded secret.
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub type Key = Zeroizing<[u8; 32]>;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ciphertext {
    version: u8,
    nonce: [u8; 12],
    data: Vec<u8>,
}

pub fn generate_key() -> Result<Key> {
    let mut key = Zeroizing::new([0u8; 32]);
    getrandom::fill(key.as_mut()).map_err(|e| anyhow::anyhow!("生成加密密钥失败: {e}"))?;
    Ok(key)
}

pub fn encrypt(key: &[u8; 32], plaintext: &[u8], context: &[u8]) -> Result<Ciphertext> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| anyhow::anyhow!("加密密钥无效"))?;
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|e| anyhow::anyhow!("生成加密随机数失败: {e}"))?;
    let data = cipher
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: plaintext,
                aad: context,
            },
        )
        .map_err(|_| anyhow::anyhow!("数据加密失败"))?;
    Ok(Ciphertext {
        version: 1,
        nonce,
        data,
    })
}

pub fn decrypt(
    key: &[u8; 32],
    ciphertext: &Ciphertext,
    context: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    ensure!(ciphertext.version == 1, "不支持的密文版本");
    ensure!(ciphertext.data.len() >= 16, "密文不完整");
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| anyhow::anyhow!("解密密钥无效"))?;
    Ok(Zeroizing::new(
        cipher
            .decrypt(
                &Nonce::from(ciphertext.nonce),
                Payload {
                    msg: &ciphertext.data,
                    aad: context,
                },
            )
            .map_err(|_| anyhow::anyhow!("密文校验失败，无法解密"))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persisted_ciphertext_recovers_original_with_fresh_reader() {
        let key = generate_key().unwrap();
        let first = encrypt(&key, "Ab123456密码".as_bytes(), b"connection-password").unwrap();
        let second = encrypt(&key, "Ab123456密码".as_bytes(), b"connection-password").unwrap();
        assert_ne!(first.nonce, second.nonce);
        assert_ne!(first.data, second.data);
        let stored = serde_json::to_vec(&first).unwrap();
        assert!(!String::from_utf8_lossy(&stored).contains("Ab123456"));
        let restored: Ciphertext = serde_json::from_slice(&stored).unwrap();
        assert_eq!(
            &*decrypt(&key, &restored, b"connection-password").unwrap(),
            "Ab123456密码".as_bytes()
        );
    }
    #[test]
    fn wrong_key_context_tampering_and_truncation_are_rejected() {
        let key = generate_key().unwrap();
        let mut data = encrypt(&key, b"Ab123456", b"server").unwrap();
        assert!(decrypt(&generate_key().unwrap(), &data, b"server").is_err());
        assert!(decrypt(&key, &data, b"client").is_err());
        data.data[0] ^= 1;
        assert!(decrypt(&key, &data, b"server").is_err());
        data.data.truncate(10);
        assert!(decrypt(&key, &data, b"server").is_err());
        data.version = 2;
        assert!(decrypt(&key, &data, b"server").is_err());
    }
}
