//! Mandatory transport authentication, independent of desktop implementations.
use anyhow::Result;
use anyhow::ensure;

#[derive(Clone)]
pub struct TransportKey(String);

impl TransportKey {
    pub fn new(key: String) -> Result<Self> {
        ensure!(!key.is_empty(), "内部通信密钥不能为空");
        Ok(Self(key))
    }

    pub fn authorize(&self, supplied: Option<&str>) -> Result<()> {
        let supplied = supplied.unwrap_or("").as_bytes();
        let expected = self.0.as_bytes();
        let difference = expected
            .iter()
            .enumerate()
            .fold(supplied.len() ^ expected.len(), |diff, (i, byte)| {
                diff | usize::from(byte ^ supplied.get(i).copied().unwrap_or(0))
            });
        ensure!(
            difference == 0,
            "内部通信认证失败，请通过 LanDeskClient 连接"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_nonempty_complete_secret() {
        assert!(TransportKey::new(String::new()).is_err());
        let key = TransportKey::new("ab".repeat(32)).unwrap();
        assert!(key.authorize(Some(&"ab".repeat(32))).is_ok());
        for supplied in [None, Some(""), Some("ab"), Some("wrong")] {
            assert!(key.authorize(supplied).is_err());
        }
        assert!(
            key.authorize(Some(&format!("{}00", "ab".repeat(32))))
                .is_err()
        );
        assert!(
            key.authorize(Some(&format!("{}00", "ab".repeat(31))))
                .is_err()
        );
    }
}
