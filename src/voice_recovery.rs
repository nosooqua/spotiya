//! Guard the activation spotter briefly after our cancellation. A late native
//! response can stop it after the cancelled request has already returned to IDLE.
use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

#[derive(Default)]
struct State {
    idle: Option<bool>,
    request: Option<String>,
    generation: u64,
}
#[derive(Default)]
pub struct Activation(Mutex<State>);
impl Activation {
    pub fn observe(&self, idle: Option<bool>, request: Option<&str>) {
        let Ok(mut state) = self.0.lock() else {
            return;
        };
        if idle.is_none()
            || (idle == Some(false)
                && (state.idle != Some(false)
                    || matches!((&state.request, request), (Some(old), Some(new)) if old != new)))
        {
            state.generation = state.generation.wrapping_add(1);
        }
        state.idle = idle;
        state.request = request.map(str::to_owned);
    }
    pub fn lease(&self, request: Option<&str>) -> Option<u64> {
        let state = self.0.lock().ok()?;
        (request.is_some() && request == state.request.as_deref() && state.idle == Some(false))
            .then_some(state.generation)
    }
    fn idle(&self, generation: u64) -> Option<bool> {
        let state = self.0.lock().ok()?;
        (state.generation == generation)
            .then_some(state.idle)
            .flatten()
    }
    pub async fn restore<F, Fut>(self: Arc<Self>, generation: u64, mut start: F)
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = io::Result<()>>,
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(500)).await;
            match self.idle(generation) {
                // start() respects the native enabled/microphone gates; enable() is never used.
                Some(true) => {
                    if start().await.is_err() {
                        break;
                    }
                }
                Some(false) => {}
                None => break, // New conversation or lost IPC: leave it entirely to Alice.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[tokio::test(start_paused = true)]
    async fn late_response_recovery_is_bounded_and_stops_for_the_next_dialog() {
        let activation = Arc::new(Activation::default());
        assert_eq!(activation.lease(Some("one")), None);
        activation.observe(Some(false), Some("one"));
        let lease = activation.lease(Some("one")).unwrap();
        assert_eq!(activation.lease(Some("old")), None);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let monitor = activation.clone();
        let task = tokio::spawn(async move {
            monitor
                .restore(lease, || {
                    count.fetch_add(1, Ordering::SeqCst);
                    async { Ok(()) }
                })
                .await;
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 0); // Still listening/thinking.
        activation.observe(Some(true), None);
        for _ in 0..3 {
            tokio::time::advance(Duration::from_millis(500)).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3); // Covers a delayed stop after initial IDLE.
        activation.observe(Some(false), Some("two"));
        activation.observe(Some(true), None); // Even a fast completed new question revokes the lease.
        tokio::time::advance(Duration::from_secs(1)).await;
        task.await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        activation.observe(Some(false), Some("three"));
        let lease = activation.lease(Some("three")).unwrap();
        activation.observe(Some(true), None);
        let at = Instant::now();
        activation.clone().restore(lease, || async { Ok(()) }).await;
        assert_eq!(at.elapsed(), Duration::from_secs(10));
        activation.observe(None, None);
        assert_eq!(activation.idle(lease), None);
    }
}
