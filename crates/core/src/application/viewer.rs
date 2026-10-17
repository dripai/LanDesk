use tokio::{
    sync::watch,
    time::{Duration, Instant},
};
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
pub struct Activity {
    viewers: usize,
    idle_since: Instant,
}

impl Activity {
    pub fn deadline(self) -> Option<Instant> {
        (self.viewers == 0).then_some(self.idle_since + IDLE_TIMEOUT)
    }
}

pub fn activity() -> (watch::Sender<Activity>, watch::Receiver<Activity>) {
    watch::channel(Activity {
        viewers: 0,
        idle_since: Instant::now(),
    })
}

pub struct Viewer(watch::Sender<Activity>);
impl Viewer {
    pub fn new(activity: watch::Sender<Activity>) -> Self {
        activity.send_modify(|state| state.viewers += 1);
        Self(activity)
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.0.send_modify(|state| {
            state.viewers -= 1;
            if state.viewers == 0 {
                state.idle_since = Instant::now();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn only_last_viewer_starts_idle_deadline_and_refresh_resets_it() {
        let (activity, state) = activity();
        assert_eq!(
            state.borrow().deadline(),
            Some(Instant::now() + IDLE_TIMEOUT)
        );
        let first = Viewer::new(activity.clone());
        let second = Viewer::new(activity.clone());
        tokio::time::advance(Duration::from_secs(120)).await;
        assert!(state.borrow().deadline().is_none());
        drop(first);
        assert!(state.borrow().deadline().is_none());
        drop(second);
        let deadline = state.borrow().deadline().unwrap();
        tokio::time::advance(Duration::from_secs(29)).await;
        assert!(Instant::now() < deadline);
        let refreshed = Viewer::new(activity);
        assert!(state.borrow().deadline().is_none());
        drop(refreshed);
        assert_eq!(
            state.borrow().deadline(),
            Some(Instant::now() + IDLE_TIMEOUT)
        );
    }
}
