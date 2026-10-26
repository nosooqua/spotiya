use std::{
    fs::OpenOptions,
    io,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;

const HOLD: Duration = Duration::from_secs(60);
#[cfg(test)]
const LEVEL: f64 = 0.2;

// A single aligned atomic word shared with the process-local AirPlay PCM filter.
// The absolute monotonic deadline also restores gain if the receiver stops updating it.
struct SharedDeadline(*mut AtomicU32);
unsafe impl Send for SharedDeadline {}
impl Drop for SharedDeadline {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.0.cast(), 4);
        }
    }
}
fn monotonic_ms() -> u32 {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    unsafe {
        if libc::clock_gettime(libc::CLOCK_MONOTONIC, time.as_mut_ptr()) != 0 {
            return 0;
        }
        let time = time.assume_init();
        (time.tv_sec as u64 * 1000 + time.tv_nsec as u64 / 1_000_000) as u32
    }
}

pub struct Ducking {
    origin: Instant,
    until: AtomicU32,
    airplay: Mutex<Option<SharedDeadline>>,
    pub volume: crate::volume_guard::VolumeGuard,
}
impl Ducking {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
            until: AtomicU32::new(0),
            airplay: Mutex::new(None),
            volume: crate::volume_guard::VolumeGuard::new(),
        }
    }
    pub fn set_busy(&self, busy: bool) {
        let until = if busy {
            (self.origin.elapsed().as_millis() as u32)
                .wrapping_add(HOLD.as_millis() as u32)
                .max(1)
        } else {
            0
        };
        self.until.store(until, Ordering::Release);
        if let Ok(mapping) = self.airplay.lock()
            && let Some(mapping) = &*mapping
        {
            let deadline = if busy {
                monotonic_ms().wrapping_add(HOLD.as_millis() as u32).max(1)
            } else {
                0
            };
            unsafe { &*mapping.0 }.store(deadline, Ordering::Release);
        }
        self.volume.activity(busy);
    }
    pub fn attach_airplay(&self, path: &Path) -> io::Result<()> {
        let mut slot = self
            .airplay
            .lock()
            .map_err(|_| io::Error::other("AirPlay ducking lock"))?;
        let temporary = path.with_extension("new");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.set_len(4)?;
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                4,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let mapping = SharedDeadline(address.cast());
        let deadline = if self.active() {
            let remaining = self
                .until
                .load(Ordering::Acquire)
                .wrapping_sub(self.origin.elapsed().as_millis() as u32);
            monotonic_ms().wrapping_add(remaining).max(1)
        } else {
            0
        };
        unsafe { &*mapping.0 }.store(deadline, Ordering::Release);
        std::fs::rename(temporary, path)?;
        *slot = Some(mapping);
        Ok(())
    }
    pub fn active(&self) -> bool {
        let until = self.until.load(Ordering::Acquire);
        until != 0 && until.wrapping_sub(self.origin.elapsed().as_millis() as u32) as i32 > 0
    }
}

pub struct Gain(f64);
unsafe extern "C" {
    fn spotiya_duck_f64(gain: *mut f64, samples: *mut f64, frames: usize, rate: u32, busy: i32);
}
impl Gain {
    pub fn new() -> Self {
        Self(1.0)
    }
    pub fn apply(&mut self, samples: &mut [f64], busy: bool) {
        unsafe {
            spotiya_duck_f64(
                &mut self.0,
                samples.as_mut_ptr(),
                samples.len() / 2,
                librespot::playback::SAMPLE_RATE,
                busy.into(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn airplay_mapping_tracks_activity_and_restarts_during_a_dialog() {
        let directory = std::env::temp_dir().join(format!("spotiya-duck-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("ducking");
        let duck = Ducking::new();
        let deadline = || u32::from_ne_bytes(std::fs::read(&path).unwrap().try_into().unwrap());
        duck.attach_airplay(&path).unwrap();
        assert_eq!(deadline(), 0);
        duck.set_busy(true);
        assert!((59000..=60000).contains(&deadline().wrapping_sub(monotonic_ms())));
        duck.attach_airplay(&path).unwrap();
        assert!((59000..=60000).contains(&deadline().wrapping_sub(monotonic_ms())));
        duck.set_busy(false);
        assert_eq!(deadline(), 0);
        drop(duck);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn idle_disconnect_and_missing_idle_event_restore_volume() {
        let duck = Ducking::new();
        assert!(!duck.active());
        duck.set_busy(true);
        assert!(duck.active());
        tokio::time::advance(HOLD - Duration::from_secs(1)).await;
        assert!(duck.active());
        duck.set_busy(true);
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(duck.active());
        duck.set_busy(false);
        assert!(!duck.active());
        duck.set_busy(true);
        tokio::time::advance(HOLD).await;
        assert!(!duck.active());
    }
    #[test]
    fn ramps_are_stereo_linked_bounded_and_independent_of_packet_size() {
        let mut gain = Gain::new();
        let mut whole = vec![1.0; 10000];
        gain.apply(&mut whole, true);
        assert!(whole[0] < 1.0 && whole[0] > 0.99);
        assert_eq!(whole[9999], LEVEL);
        assert!(whole.windows(2).all(|w| w[0] >= w[1]));
        assert!(whole.as_chunks::<2>().0.iter().all(|f| f[0] == f[1]));
        let mut split = vec![1.0; 10000];
        let mut split_gain = Gain::new();
        for chunk in split.chunks_mut(128) {
            split_gain.apply(chunk, true);
        }
        assert_eq!(whole, split);
        let mut restored = vec![1.0; 30000];
        gain.apply(&mut restored, false);
        assert!(restored[0] > LEVEL && restored[0] < 0.21);
        assert_eq!(restored[29999], 1.0);
        assert!(restored.windows(2).all(|w| w[0] <= w[1]));
        let original = vec![-0.5, 0.5, 0.0, 1.0];
        let mut untouched = original.clone();
        gain.apply(&mut untouched, false);
        assert_eq!(untouched, original);
    }
}
