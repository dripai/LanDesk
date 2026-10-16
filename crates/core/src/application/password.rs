//! Password and authentication use cases; persistence and OS key protection are ports.
use crate::domain::identity::{CommitFailure, CommitResult, CommitStatus};
use crate::{
    domain::{
        identity::{PASSWORD_PURPOSE, ServerIdentity},
        password,
    },
    ports::{IdentityRepository, SecretProtector},
    security::secret::Secret,
};
use anyhow::{Context, Result, ensure};
use zeroize::Zeroizing;

pub enum PasswordOutcome {
    Saved {
        password: Zeroizing<String>,
        warning: Option<String>,
    },
    Failed(CommitFailure),
}
pub fn change(
    repository: &dyn IdentityRepository,
    protector: &dyn SecretProtector,
    password: Zeroizing<String>,
) -> PasswordOutcome {
    save(repository, protector, password, None)
}
pub fn change_connection(
    repository: &dyn IdentityRepository,
    protector: &dyn SecretProtector,
    password: Zeroizing<String>,
    port: u16,
) -> PasswordOutcome {
    save(repository, protector, password, Some(port))
}
fn save(
    repository: &dyn IdentityRepository,
    protector: &dyn SecretProtector,
    password: Zeroizing<String>,
    port: Option<u16>,
) -> PasswordOutcome {
    let run = || -> CommitResult {
        password::validate(&password)?;
        if port == Some(0) {
            return Err(anyhow::anyhow!("端口必须在 1–65535 之间").into());
        }
        let _operation = repository.lock()?;
        let mut identity = repository.read()?;
        let secret = Secret::encrypt(password.as_bytes(), PASSWORD_PURPOSE, protector)?;
        identity.replace_credential(&password, secret)?;
        if let Some(port) = port {
            identity.port = port;
        }
        repository.commit(&identity)
    };
    match run() {
        Ok(status) => PasswordOutcome::Saved {
            password,
            warning: match status {
                CommitStatus::Committed => None,
                CommitStatus::CommittedWithWarning(w) => Some(w),
            },
        },
        Err(e) => PasswordOutcome::Failed(e),
    }
}
pub fn authenticate(
    repository: &dyn IdentityRepository,
    running: &ServerIdentity,
    password: &str,
) -> Result<bool> {
    let current = repository.read()?;
    ensure!(
        current.same_host(running),
        "运行期间连接身份发生变化，请重新启动 LanDeskServer"
    );
    current.verify_password(password)
}

pub fn recover_password(
    identity: &ServerIdentity,
    protector: &dyn SecretProtector,
) -> Result<Zeroizing<String>> {
    let bytes = identity
        .password_secret()
        .decrypt(PASSWORD_PURPOSE, protector)?;
    let password =
        Zeroizing::new(String::from_utf8(bytes.to_vec()).context("连接密码解密后编码无效")?);
    password::validate(&password)?;
    ensure!(
        identity.verify_password(&password)?,
        "显示密码与连接校验配置不一致"
    );
    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };
    struct Protector {
        writes: AtomicUsize,
        fail: bool,
    }
    impl SecretProtector for Protector {
        fn key_for_write(&self, _: &str) -> Result<crate::security::encryption::Key> {
            self.writes.fetch_add(1, Ordering::Relaxed);
            ensure!(!self.fail, "injected key protection failure");
            Ok(Zeroizing::new([7; 32]))
        }
        fn key_for_read(&self, _: &str) -> Result<crate::security::encryption::Key> {
            Ok(Zeroizing::new([7; 32]))
        }
    }
    struct Lease(Arc<AtomicBool>);
    impl Drop for Lease {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    struct Repository {
        identity: RefCell<ServerIdentity>,
        lock: Arc<AtomicBool>,
        outcome: u8,
    }
    impl IdentityRepository for Repository {
        fn lock(&self) -> Result<Box<dyn std::any::Any>> {
            ensure!(!self.lock.swap(true, Ordering::AcqRel), "busy");
            Ok(Box::new(Lease(self.lock.clone())))
        }
        fn read(&self) -> Result<ServerIdentity> {
            Ok(serde_json::from_slice(&serde_json::to_vec(
                &*self.identity.borrow(),
            )?)?)
        }
        fn commit(&self, identity: &ServerIdentity) -> CommitResult {
            match self.outcome {
                1 => Err(CommitFailure::BeforeCommit(anyhow::anyhow!(
                    "injected write failure"
                ))),
                2 => Err(CommitFailure::Unknown(anyhow::anyhow!(
                    "injected rollback failure"
                ))),
                _ => {
                    *self.identity.borrow_mut() =
                        serde_json::from_slice(&serde_json::to_vec(identity)?)?;
                    Ok(if self.outcome == 3 {
                        CommitStatus::CommittedWithWarning("cleanup failed".into())
                    } else {
                        CommitStatus::Committed
                    })
                }
            }
        }
    }
    fn repository(outcome: u8) -> Repository {
        Repository {
            identity: RefCell::new(
                ServerIdentity::new(
                    "old-password",
                    2222,
                    Secret::for_test(b"old-password", PASSWORD_PURPOSE),
                    "host-identity".into(),
                    "ab".repeat(32),
                )
                .unwrap(),
            ),
            lock: Arc::default(),
            outcome,
        }
    }
    #[test]
    fn saved_password_and_ciphertext_change_together_without_changing_host_identity() {
        let repo = repository(0);
        let old = repo.read().unwrap();
        let protector = Protector {
            writes: AtomicUsize::new(0),
            fail: false,
        };
        assert!(matches!(
            change(&repo, &protector, Zeroizing::new("new-password".into())),
            PasswordOutcome::Saved { warning: None, .. }
        ));
        let saved = repo.read().unwrap();
        assert!(saved.same_host(&old));
        assert!(saved.verify_password("new-password").unwrap());
        assert!(!saved.verify_password("old-password").unwrap());
        assert_eq!(
            recover_password(&saved, &protector).unwrap().as_str(),
            "new-password"
        );
        assert!(!repo.lock.load(Ordering::Acquire));
    }
    #[test]
    fn failures_before_commit_leave_old_identity_and_release_the_lock() {
        for (outcome, key_failure, password) in
            [(0, false, ""), (0, true, "new"), (1, false, "new")]
        {
            let repo = repository(outcome);
            let protector = Protector {
                writes: AtomicUsize::new(0),
                fail: key_failure,
            };
            assert!(matches!(
                change(&repo, &protector, Zeroizing::new(password.into())),
                PasswordOutcome::Failed(CommitFailure::BeforeCommit(_))
            ));
            assert!(
                repo.read()
                    .unwrap()
                    .verify_password("old-password")
                    .unwrap()
            );
            assert!(!repo.lock.load(Ordering::Acquire));
            if password.is_empty() {
                assert_eq!(protector.writes.load(Ordering::Relaxed), 0);
            }
        }
    }
    #[test]
    fn cleanup_failure_is_saved_but_rollback_failure_is_unknown() {
        let protector = Protector {
            writes: AtomicUsize::new(0),
            fail: false,
        };
        let repo = repository(3);
        assert!(matches!(
            change(&repo, &protector, Zeroizing::new("new".into())),
            PasswordOutcome::Saved {
                warning: Some(_),
                ..
            }
        ));
        assert!(repo.read().unwrap().verify_password("new").unwrap());
        assert!(matches!(
            change(&repository(2), &protector, Zeroizing::new("new".into())),
            PasswordOutcome::Failed(CommitFailure::Unknown(_))
        ));
    }
    #[test]
    fn connection_settings_commit_together_and_invalid_port_changes_nothing() {
        let repo = repository(0);
        let protector = Protector {
            writes: AtomicUsize::new(0),
            fail: false,
        };
        let original_port = repo.read().unwrap().port;
        assert!(matches!(
            change_connection(&repo, &protector, Zeroizing::new("new".into()), 0),
            PasswordOutcome::Failed(_)
        ));
        assert_eq!(repo.read().unwrap().port, original_port);
        assert!(
            repo.read()
                .unwrap()
                .verify_password("old-password")
                .unwrap()
        );
        assert!(matches!(
            change_connection(&repo, &protector, Zeroizing::new("new".into()), 2345),
            PasswordOutcome::Saved { .. }
        ));
        let saved = repo.read().unwrap();
        assert_eq!(saved.port, 2345);
        assert!(saved.verify_password("new").unwrap());
        assert_eq!(&*recover_password(&saved, &protector).unwrap(), "new");
    }
    #[test]
    fn concurrent_operation_and_changed_running_identity_are_rejected() {
        let repo = repository(0);
        let protector = Protector {
            writes: AtomicUsize::new(0),
            fail: false,
        };
        let _lock = repo.lock().unwrap();
        assert!(matches!(
            change(&repo, &protector, Zeroizing::new("new".into())),
            PasswordOutcome::Failed(_)
        ));
        assert_eq!(protector.writes.load(Ordering::Relaxed), 0);
        let mut running = repo.read().unwrap();
        assert!(authenticate(&repo, &running, "old-password").unwrap());
        running.port += 1;
        assert!(authenticate(&repo, &running, "old-password").is_err());
    }
}
