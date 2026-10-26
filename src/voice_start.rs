//! Queue a local load only after Spotify acknowledged activation. Cluster updates
//! may revoke that activation before Load is processed; retry that handoff, bounded.
use std::time::Duration;
use tokio::time::Instant;

#[derive(Clone, Copy)]
enum Phase {
    Activate,
    Waiting,
    Ready,
    Submitted,
}
struct Pending<T> {
    value: T,
    deadline: Instant,
    at: Instant,
    attempts: u8,
    phase: Phase,
}
pub enum Action<T> {
    Activate,
    Load(T),
    Failed,
}
pub struct Launcher<T> {
    active: bool,
    pending: Option<Pending<T>>,
}
impl<T: Clone> Default for Launcher<T> {
    fn default() -> Self {
        Self {
            active: false,
            pending: None,
        }
    }
}
impl<T: Clone> Launcher<T> {
    pub fn begin(&mut self, value: T) {
        let now = Instant::now();
        self.pending = Some(Pending {
            value,
            deadline: now + Duration::from_secs(15),
            at: now,
            attempts: 0,
            phase: if self.active {
                Phase::Ready
            } else {
                Phase::Activate
            },
        });
    }
    pub fn cancel(&mut self) {
        self.pending = None;
    }
    pub fn reset(&mut self) {
        self.cancel();
        self.active = false;
    }
    pub fn session(&mut self, active: bool) {
        self.active = active;
        if let Some(pending) = &mut self.pending {
            if !active {
                pending.phase = Phase::Activate;
                pending.at = Instant::now() + Duration::from_millis(250);
            } else if !matches!(pending.phase, Phase::Submitted) {
                pending.phase = Phase::Ready;
                // Let the cluster update following activation settle before sending Load.
                pending.at = Instant::now() + Duration::from_millis(250);
            }
        }
    }
    pub fn loading(&mut self) {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| matches!(p.phase, Phase::Submitted))
        {
            self.cancel();
        }
    }
    pub fn poll(&mut self) -> Option<Action<T>> {
        let p = self.pending.as_mut()?;
        let now = Instant::now();
        if now >= p.deadline {
            self.cancel();
            return Some(Action::Failed);
        }
        if now < p.at {
            return None;
        }
        match p.phase {
            Phase::Activate | Phase::Waiting => {
                if p.attempts >= 3 {
                    self.cancel();
                    return Some(Action::Failed);
                }
                p.attempts += 1;
                p.phase = Phase::Waiting;
                p.at = now + Duration::from_secs(2);
                Some(Action::Activate)
            }
            Phase::Ready => {
                p.phase = Phase::Submitted;
                Some(Action::Load(p.value.clone()))
            }
            Phase::Submitted => None,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn cluster_revocation_does_not_silently_drop_voice_load() {
        let mut launch = Launcher::default();
        launch.begin("first");
        assert!(matches!(launch.poll(), Some(Action::Activate)));
        assert!(launch.poll().is_none());
        launch.session(true);
        assert!(launch.poll().is_none());
        launch.session(false); // Server revokes activation before Load is handled.
        tokio::time::advance(Duration::from_millis(250)).await;
        assert!(matches!(launch.poll(), Some(Action::Activate)));
        launch.session(true);
        tokio::time::advance(Duration::from_millis(250)).await;
        assert!(matches!(launch.poll(), Some(Action::Load("first"))));
        launch.session(false); // Or immediately after the queued Load: still no Loading event.
        tokio::time::advance(Duration::from_millis(250)).await;
        assert!(matches!(launch.poll(), Some(Action::Activate)));
        launch.session(true);
        tokio::time::advance(Duration::from_millis(250)).await;
        assert!(matches!(launch.poll(), Some(Action::Load("first"))));
        launch.loading();
        launch.session(false); // Once playback starts, a remote transfer must not be reclaimed.
        tokio::time::advance(Duration::from_secs(20)).await;
        assert!(launch.poll().is_none());
    }
    #[tokio::test(start_paused = true)]
    async fn missing_ack_timeout_and_new_commands_are_bounded() {
        let mut launch = Launcher::default();
        launch.begin("one");
        for _ in 0..3 {
            assert!(matches!(launch.poll(), Some(Action::Activate)));
            tokio::time::advance(Duration::from_secs(2)).await;
        }
        assert!(matches!(launch.poll(), Some(Action::Failed)));
        assert!(launch.poll().is_none());
        launch.session(true);
        launch.begin("old");
        launch.begin("new");
        assert!(matches!(launch.poll(), Some(Action::Load("new"))));
        tokio::time::advance(Duration::from_secs(15)).await;
        assert!(matches!(launch.poll(), Some(Action::Failed)));
        launch.begin("cancelled");
        launch.cancel();
        assert!(launch.poll().is_none());
        launch.begin("lost connection");
        launch.reset();
        launch.session(true);
        assert!(launch.poll().is_none());
    }
}
