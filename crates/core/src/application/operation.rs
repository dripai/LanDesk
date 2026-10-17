//! A single owned background operation. A second submission is rejected until consumed.
use anyhow::{Context, Result, ensure};
use std::thread::JoinHandle;
#[derive(Default)]
pub struct Operation<T> {
    task: Option<JoinHandle<T>>,
}
impl<T: Send + 'static> Operation<T> {
    pub fn new() -> Self {
        Self { task: None }
    }
    pub fn is_busy(&self) -> bool {
        self.task.is_some()
    }
    pub fn start(&mut self, work: impl FnOnce() -> T + Send + 'static) -> Result<()> {
        ensure!(!self.is_busy(), "操作正在保存，请等待完成");
        self.task = Some(
            std::thread::Builder::new()
                .name("landesk-operation".into())
                .spawn(work)
                .context("无法启动保存任务")?,
        );
        Ok(())
    }
    pub fn poll(&mut self) -> Option<Result<T>> {
        if !self.task.as_ref()?.is_finished() {
            return None;
        }
        Some(
            self.task
                .take()
                .unwrap()
                .join()
                .map_err(|_| anyhow::anyhow!("保存任务异常终止，结果无法确认，请重新读取配置")),
        )
    }
}
impl<T> Drop for Operation<T> {
    fn drop(&mut self) {
        // Hosts normally drain by polling while pumping UI events. Never detach a commit on unwind.
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    #[test]
    fn blocked_work_never_blocks_poll_and_duplicate_submission_is_rejected() {
        let (release, wait) = mpsc::channel();
        let mut operation = Operation::new();
        operation
            .start(move || {
                wait.recv().unwrap();
                42
            })
            .unwrap();
        assert!(operation.poll().is_none());
        assert!(operation.start(|| 9).is_err());
        release.send(()).unwrap();
        let result = loop {
            if let Some(result) = operation.poll() {
                break result.unwrap();
            }
            std::thread::yield_now();
        };
        assert_eq!(result, 42);
        assert!(!operation.is_busy());
        operation.start(|| 9).unwrap();
    }
    #[test]
    fn dropping_owner_joins_instead_of_detaching_commit() {
        let (release, wait) = mpsc::channel();
        let (committed, complete) = mpsc::channel();
        let mut operation = Operation::new();
        operation
            .start(move || {
                wait.recv().unwrap();
                committed.send(()).unwrap();
            })
            .unwrap();
        let closer = std::thread::spawn(move || drop(operation));
        assert!(!closer.is_finished());
        release.send(()).unwrap();
        closer.join().unwrap();
        complete.try_recv().unwrap();
    }
    #[test]
    fn panic_is_reported_as_unknown_and_a_new_operation_can_start() {
        let mut operation = Operation::<()>::new();
        operation
            .start(|| panic!("injected worker failure"))
            .unwrap();
        loop {
            if let Some(result) = operation.poll() {
                assert!(result.unwrap_err().to_string().contains("无法确认"));
                break;
            }
            std::thread::yield_now();
        }
        operation.start(|| ()).unwrap();
    }
}

/// Coalesced read requests: an invalidated result can never overwrite newer UI state.
/// Writes use Operation directly and cannot be superseded this way.
pub struct Refresh<T> {
    operation: Operation<T>,
    requested: bool,
    invalidated: bool,
}
impl<T: Send + 'static> Default for Refresh<T> {
    fn default() -> Self {
        Self {
            operation: Operation::new(),
            requested: false,
            invalidated: false,
        }
    }
}
impl<T: Send + 'static> Refresh<T> {
    pub fn invalidate(&mut self) {
        self.invalidated = true;
        self.requested = false;
    }
    pub fn request(&mut self) {
        self.invalidated = true;
        self.requested = true;
    }
    pub fn poll(&mut self, read: impl FnOnce() -> T + Send + 'static) -> Option<Result<T>> {
        let completed = self.operation.poll();
        let result = if self.invalidated { None } else { completed };
        if self.requested && !self.operation.is_busy() {
            self.requested = false;
            self.invalidated = false;
            if let Err(error) = self.operation.start(read) {
                return Some(Err(error));
            }
        }
        result
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use std::sync::mpsc;
    #[test]
    fn invalidated_read_is_discarded_before_latest_read_is_started() {
        let (release, wait) = mpsc::channel();
        let mut refresh = Refresh::default();
        refresh.request();
        assert!(
            refresh
                .poll(move || {
                    wait.recv().unwrap();
                    1
                })
                .is_none()
        );
        refresh.invalidate();
        refresh.request();
        release.send(()).unwrap();
        loop {
            if let Some(value) = refresh.poll(|| 2) {
                assert_eq!(value.unwrap(), 2);
                break;
            }
            std::thread::yield_now();
        }
        assert!(refresh.poll(|| panic!("no unsolicited read")).is_none());
    }
}
