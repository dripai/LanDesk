//! Recoverable local secrets. OS protection holds the encryption key;
//! the configuration holds only authenticated ciphertext and a wrapped key.
use crate::security::encryption;
use crate::security::encryption::Ciphertext;
use anyhow::Result;

use serde::Deserialize;
use serde::Serialize;
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Secret {
    data: Ciphertext,
    wrapped_key: Vec<u8>,
}
impl Secret {
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(plaintext: &[u8], purpose: &str) -> Self {
        Self {
            data: encryption::encrypt(&[7u8; 32], plaintext, purpose.as_bytes()).unwrap(),
            wrapped_key: Vec::new(),
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn decrypt_for_test(&self, purpose: &str) -> Zeroizing<Vec<u8>> {
        encryption::decrypt(&[7u8; 32], &self.data, purpose.as_bytes()).unwrap()
    }
    pub fn encrypt(
        plaintext: &[u8],
        purpose: &str,
        protector: &dyn crate::ports::SecretProtector,
    ) -> Result<Self> {
        let (key, wrapped_key) = protector.key_for_write(purpose)?;
        Ok(Self {
            data: encryption::encrypt(&key, plaintext, purpose.as_bytes())?,
            wrapped_key,
        })
    }
    pub fn decrypt(
        &self,
        purpose: &str,
        protector: &dyn crate::ports::SecretProtector,
    ) -> Result<Zeroizing<Vec<u8>>> {
        let key = protector.key_for_read(&self.wrapped_key, purpose)?;
        encryption::decrypt(&key, &self.data, purpose.as_bytes())
    }
}
