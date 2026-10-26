//! Shared transport routing for physical controls, voice and the local CLI.
use crate::{Shared, airplay, leds, voice::Intent};
use librespot::connect::Spirc;
use std::sync::atomic::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Spotify,
    Airplay(u32),
}
impl Source {
    pub fn current(shared: &Shared) -> Self {
        if shared.airplay.active.load(Ordering::Acquire) {
            Self::Airplay(shared.airplay.remote_session.load(Ordering::Acquire))
        } else {
            Self::Spotify
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
}
impl Command {
    pub fn from_intent(intent: &Intent) -> Option<Self> {
        match intent {
            Intent::Resume => Some(Self::Play),
            Intent::Pause => Some(Self::Pause),
            Intent::Toggle => Some(Self::Toggle),
            Intent::Next => Some(Self::Next),
            Intent::Previous => Some(Self::Previous),
            _ => None,
        }
    }
    pub fn airplay_code(self) -> u8 {
        match self {
            Self::Play => 0,
            Self::Pause => 1,
            Self::Toggle => 2,
            Self::Next => 4,
            Self::Previous => 5,
        }
    }
}
pub fn button(action: i32, volume: u16, step: u32) -> Option<Intent> {
    match action {
        0 => Some(Intent::Toggle),
        1 => Some(Intent::Next),
        2 => Some(Intent::Previous),
        3 | 4 => Some(Intent::Volume(
            (i64::from(volume)
                + if action == 3 {
                    i64::from(step)
                } else {
                    -i64::from(step)
                })
            .clamp(0, 100) as u16,
        )),
        _ => None,
    }
}
pub async fn execute(
    command: Command,
    source: Source,
    shared: &Shared,
    spotify: Option<&Spirc>,
    airplay: &airplay::Runtime,
) -> bool {
    if !shared.active.load(Ordering::Acquire) || source != Source::current(shared) {
        return false;
    }
    let result = match source {
        Source::Airplay(session) => airplay.command(session, command, &shared.airplay).await,
        Source::Spotify => match spotify {
            Some(handle) => match command {
                Command::Play => handle.play(),
                Command::Pause => handle.pause(),
                Command::Next => handle.next(),
                Command::Previous => handle.prev(),
                Command::Toggle => handle.play_pause(),
            }
            .map_err(std::io::Error::other),
            None => return false,
        },
    };
    if let Err(error) = result {
        eprintln!("transport: {error}");
        return false;
    }
    match command {
        Command::Next => shared.feedback.show(leds::Effect::Next),
        Command::Previous => shared.feedback.show(leds::Effect::Previous),
        _ => {}
    }
    true
}
pub fn set_volume(
    volume: u16,
    mixer: &crate::DeviceMixer,
    shared: &Shared,
    spotify: Option<&Spirc>,
) -> bool {
    use librespot::playback::mixer::Mixer;
    mixer.set_master_volume(crate::from_percent(volume));
    if Source::current(shared) == Source::Spotify
        && let Some(handle) = spotify
    {
        handle.set_volume(mixer.volume()).is_ok()
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn airplay_transport_works_without_spotify_and_never_routes_a_stale_source() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicU16},
        };
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let dir = std::env::temp_dir().join(format!("transport-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let ducking = Arc::new(crate::ducking::Ducking::new());
        let mut airplay = airplay::Runtime::new(
            &dir,
            &dir,
            "Test",
            &crate::profiles::Audio::default(),
            ducking.clone(),
        );
        airplay.test_runtime(&dir);
        let shared = Shared {
            running: AtomicBool::new(true),
            controls_enabled: AtomicBool::new(true),
            active: AtomicBool::new(true),
            paused: AtomicBool::new(false),
            volume: AtomicU16::new(0),
            commands: tokio::sync::mpsc::unbounded_channel().0,
            spectrum: Arc::new(crate::spectrum::Spectrum::new(false)),
            feedback: leds::Feedback::default(),
            volume_cue: crate::volume_sound::Cue::default(),
            airplay: Arc::new(airplay::Gate::new()),
            ducking,
        };
        shared.airplay.active.store(true, Ordering::Release);
        shared.airplay.remote_session.store(7, Ordering::Release);
        shared
            .airplay
            .remote_available
            .store(true, Ordering::Release);
        let listener = tokio::net::UnixListener::bind(dir.join("control")).unwrap();
        let server = tokio::spawn(async move {
            for command in [2, 2, 4, 5] {
                let (socket, _) = listener.accept().await.unwrap();
                let mut reader = tokio::io::BufReader::new(socket);
                let mut request = String::new();
                reader.read_line(&mut request).await.unwrap();
                assert_eq!(request, format!("COMMAND 7 {command}\n"));
                reader.get_mut().write_all(b"OK\n").await.unwrap();
            }
        });
        assert!(!execute(Command::Next, Source::Spotify, &shared, None, &airplay).await);
        assert!(!execute(Command::Next, Source::Airplay(6), &shared, None, &airplay).await);
        for (command, paused) in [
            (Command::Toggle, false),
            (Command::Toggle, true),
            (Command::Next, false),
            (Command::Previous, false),
        ] {
            shared.paused.store(paused, Ordering::Relaxed);
            assert!(execute(command, Source::Airplay(7), &shared, None, &airplay).await);
            assert!(shared.airplay.active.load(Ordering::Acquire));
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn buttons_use_the_same_intents_as_voice_and_bound_volume() {
        for (button, phrase, command) in [
            (1, "следующий трек", Command::Next),
            (2, "предыдущий трек", Command::Previous),
        ] {
            let intent = button_intent(button);
            assert_eq!(Some(intent.clone()), crate::voice::intent(phrase));
            assert_eq!(Command::from_intent(&intent), Some(command));
        }
        assert_eq!(button(3, 99, 5), Some(Intent::Volume(100)));
        assert_eq!(button(4, 1, 5), Some(Intent::Volume(0)));
    }
    fn button_intent(action: i32) -> Intent {
        button(action, 50, 5).unwrap()
    }
}
