//! Recoverable local secrets. OS protection holds the encryption key;
//! the configuration holds only authenticated ciphertext and a wrapped key.
use crate::encryption::{self, Ciphertext, Key};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Secret {
    data: Ciphertext,
    wrapped_key: Vec<u8>,
}
impl Secret {
    #[cfg(test)]
    pub(crate) fn for_test(plaintext: &[u8], purpose: &str) -> Self {
        Self {
            data: encryption::encrypt(&[7u8; 32], plaintext, purpose.as_bytes()).unwrap(),
            wrapped_key: Vec::new(),
        }
    }
    #[cfg(test)]
    pub(crate) fn decrypt_for_test(&self, purpose: &str) -> Zeroizing<Vec<u8>> {
        encryption::decrypt(&[7u8; 32], &self.data, purpose.as_bytes()).unwrap()
    }
    pub fn encrypt(plaintext: &[u8], purpose: &str) -> Result<Self> {
        let (key, wrapped_key) = platform::key_for_write(purpose)?;
        Ok(Self {
            data: encryption::encrypt(&key, plaintext, purpose.as_bytes())?,
            wrapped_key,
        })
    }
    pub fn decrypt(&self, purpose: &str) -> Result<Zeroizing<Vec<u8>>> {
        let key = platform::key_for_read(&self.wrapped_key, purpose)?;
        encryption::decrypt(&key, &self.data, purpose.as_bytes())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use anyhow::Context;
    use core_foundation::data::CFData;
    use security_framework::{
        item::{ItemAddOptions, ItemAddValue, ItemClass},
        passwords::get_generic_password,
    };
    const SERVICE: &str = "local.aiwork.LanDesk.Encryption";
    // errSecItemNotFound: only a first write may create the key. A failed read
    // must not create a different key or silently discard stored passwords.
    const NOT_FOUND: i32 = -25300;
    fn read(purpose: &str) -> Result<Key> {
        let bytes = Zeroizing::new(
            get_generic_password(SERVICE, purpose).context("无法读取钥匙串中的加密密钥")?,
        );
        ensure!(bytes.len() == 32, "钥匙串加密密钥长度无效");
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&bytes);
        Ok(key)
    }
    pub fn key_for_write(purpose: &str) -> Result<(Key, Vec<u8>)> {
        match get_generic_password(SERVICE, purpose) {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                ensure!(bytes.len() == 32, "钥匙串加密密钥长度无效");
                let mut key = Zeroizing::new([0u8; 32]);
                key.copy_from_slice(&bytes);
                Ok((key, Vec::new()))
            }
            Err(error) if error.code() == NOT_FOUND => {
                let key = encryption::generate_key()?;
                let mut item = ItemAddOptions::new(ItemAddValue::Data {
                    class: ItemClass::generic_password(),
                    data: CFData::from_buffer(key.as_ref()),
                });
                item.set_service(SERVICE).set_account_name(purpose);
                match item.add() {
                    Ok(()) => ensure!(*read(purpose)? == *key, "钥匙串密钥保存后核验失败"),
                    Err(error) if error.code() == -25299 => {} // Another launch created it; never overwrite that key.
                    Err(error) => return Err(error).context("无法保存钥匙串加密密钥"),
                }
                Ok((read(purpose)?, Vec::new()))
            }
            Err(error) => Err(error).context("无法读取钥匙串中的加密密钥"),
        }
    }
    pub fn key_for_read(wrapped: &[u8], purpose: &str) -> Result<Key> {
        ensure!(wrapped.is_empty(), "密文不属于 macOS 钥匙串存储");
        read(purpose)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn dpapi_wrapped_key_restores_persisted_password_and_rejects_damage() {
        let encrypted = Secret::encrypt(b"aB123456", "LanDesk.SecretStorage.Test").unwrap();
        let stored = serde_json::to_vec(&encrypted).unwrap();
        let mut restored: Secret = serde_json::from_slice(&stored).unwrap();
        assert_eq!(
            &*restored.decrypt("LanDesk.SecretStorage.Test").unwrap(),
            b"aB123456"
        );
        assert!(restored.decrypt("different-purpose").is_err());
        restored.wrapped_key[0] ^= 1;
        assert!(restored.decrypt("LanDesk.SecretStorage.Test").is_err());
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use anyhow::Context;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{Foundation::LocalFree, Security::Cryptography::*};
    struct Blob(CRYPT_INTEGER_BLOB);
    impl Drop for Blob {
        fn drop(&mut self) {
            if !self.0.pbData.is_null() {
                unsafe {
                    use zeroize::Zeroize;
                    std::slice::from_raw_parts_mut(self.0.pbData, self.0.cbData as usize).zeroize();
                    LocalFree(self.0.pbData.cast());
                }
            }
        }
    }
    fn dpapi(input: &[u8], purpose: &str, encrypt: bool) -> Result<Zeroizing<Vec<u8>>> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: input.len().try_into()?,
            pbData: input.as_ptr().cast_mut(),
        };
        let entropy = CRYPT_INTEGER_BLOB {
            cbData: purpose.len().try_into()?,
            pbData: purpose.as_ptr().cast_mut(),
        };
        let mut output = Blob(CRYPT_INTEGER_BLOB::default());
        let result = unsafe {
            if encrypt {
                CryptProtectData(
                    &input,
                    null(),
                    &entropy,
                    null(),
                    null(),
                    CRYPTPROTECT_LOCAL_MACHINE | CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output.0,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    null_mut(),
                    &entropy,
                    null(),
                    null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output.0,
                )
            }
        };
        if result == 0 {
            return Err(std::io::Error::last_os_error()).context("Windows 密钥保护操作失败");
        }
        ensure!(
            !output.0.pbData.is_null() && output.0.cbData <= 4096,
            "Windows 返回了无效的加密数据"
        );
        Ok(Zeroizing::new(
            unsafe { std::slice::from_raw_parts(output.0.pbData, output.0.cbData as usize) }
                .to_vec(),
        ))
    }
    pub fn key_for_write(purpose: &str) -> Result<(Key, Vec<u8>)> {
        let key = encryption::generate_key()?;
        let wrapped = dpapi(key.as_ref(), purpose, true)?.to_vec();
        Ok((key, wrapped))
    }
    pub fn key_for_read(wrapped: &[u8], purpose: &str) -> Result<Key> {
        ensure!(
            !wrapped.is_empty() && wrapped.len() <= 4096,
            "Windows 加密密钥无效"
        );
        let bytes = dpapi(wrapped, purpose, false)?;
        ensure!(bytes.len() == 32, "Windows 解密密钥长度无效");
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&bytes);
        Ok(key)
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use super::*;
    pub fn key_for_write(_: &str) -> Result<(Key, Vec<u8>)> {
        anyhow::bail!("当前平台未实现本地密钥存储")
    }
    pub fn key_for_read(_: &[u8], _: &str) -> Result<Key> {
        anyhow::bail!("当前平台未实现本地密钥存储")
    }
}
